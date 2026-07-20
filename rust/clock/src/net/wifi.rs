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

// #142: WiFi creds are read at runtime from the single baked `WIFI_NETWORK` (ssid/pass) inside
// `associate` — no fixed SSID const, and (post-#142) no slot selection or un-brickable fallback.

/// NTP server IPv4. We hardcode an anycast IP so we need no DNS resolver in
/// the smoltcp build. time.cloudflare.com's NTP anycast address:
const NTP_SERVER_IP: Ipv4Addr = Ipv4Addr::new(162, 159, 200, 123);
const NTP_PORT: u16 = 123;

// #100 HA Mosquitto broker (v2 MQTT-native bridge): the leg is now the ACTIVE slot's own-VLAN
// broker, resolved at RUNTIME in `mqtt_session` from the NVS net-record (`active_broker()`) — a
// slot IS a (ssid, broker, ota) tuple, so the broker MUST follow the associated network (the
// quad-homed-broker rule: a cross-VLAN leg drops CONNACK). Not a compile-time const any more.

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

/// #155 channel-drag OPERATOR LEVER: a retained hint the crown HONORS at claim time.
/// Payload = a decimal 2.4 GHz channel (the fleet uses `1`/`6`/`11`); an EMPTY payload (the
/// retain-clear) restores un-hinted behavior. The mesh channel is PHYSICALLY the crown's AP
/// channel (coexist single-radio: while associated the PHY sits on the AP's channel), so a hint
/// can only steer WHICH board holds the crown — a candidate whose LEARNED channel != the hint
/// refuses to claim (see the claim gate in `mqtt_session`), so the (re)election converges onto a
/// board already on the hinted channel and the fleet stops being dragged onto a weak AP. This
/// replaces JP's manual seq-forged `MC` plant with a first-class, documented control. Absent/empty
/// ⇒ the election is byte-identical to pre-#155. See issue #155 (option 3). SAFETY: an
/// unsatisfiable hint (no capable board on that channel) leaves the mesh crownless until the
/// operator clears the topic — the lever is deliberate, not automatic.
#[cfg(feature = "wifi")]
const MESH_CHANNEL_HINT_TOPIC: &[u8] = b"smol/mesh/channel_hint";

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
    // --- #51 inputs (seeded by the caller) ---
    /// This board's live RSSI-to-AP (dBm, signed; weaker = more negative). Captured
    /// after association and persisted on the RadioManager. Consumed by the #51 leaf
    /// RECOVERY election so the strongest-uplink survivor takes over a dead owner
    /// FIRST (weaker boards defer + adopt it). Ignored on the boot/flush paths.
    pub my_rssi: i8,
    /// #29: the owner's LEARNED ESP-NOW channel (from `rx_control`; 0 until known). Seeded by the
    /// caller from the live RadioManager; written into the retained `MC|owner|<ch>|seq` record when
    /// this board publishes as owner, so a roaming/re-electing leaf can pre-tune to it instead of
    /// scanning 1/6/11. ADVISORY: `0` ⇒ leaves keep HELLO-scanning (the proven fallback). The
    /// election destructures the channel field as `_ch` (ignored), so this can never perturb it.
    pub my_channel: u8,
    /// #51: true ONLY on a LEAF's recovery re-election (a lost owner). Selects the
    /// WiFi-strength "sticky live owner + RSSI-weighted takeover" rule. On boot and
    /// gateway-flush this is false → the original, hardware-validated lowest-id
    /// election runs UNCHANGED (preserves #2 split-brain + fast cold-start).
    pub recovery: bool,
    /// #114 H1: (recovery only) true iff this leaf has NEVER heard a HELLO from the owner it is
    /// following. A DEAD owner (frozen seq) that was never heard is a forged / phantom retained MC
    /// (the crown-handover standoff) — there is no live board to stagger against, so the resolver
    /// takes it over PROMPTLY (id-only tiebreak) instead of waiting the full RSSI backoff. Never
    /// overrides the FROZEN-seq safety gate: an owner whose seq still advances is alive and is never
    /// taken over regardless of this flag (RF-dead-zone protection intact).
    pub owner_never_heard: bool,
    /// #136: (recovery only) a floor for the HEARD-path takeover window = the worst-case gap
    /// between a LIVE owner's *observed* MC seq advances (`RELAY_FLUSH_INTERVAL_MS` + a slow/failed
    /// flush bounded by `RELAY_FLUSH_BUDGET`). The caller (espnow tier) computes it from those two
    /// constants and seeds it here so the wifi-tier resolver can honor it without a cross-cfg
    /// dependency. The resolver takes over a heard-then-lost owner only past
    /// `max(RECOVERY_STALE_MS, recovery_stale_floor_ms)`, so a gateway that republishes within a
    /// flush-interval-plus-budget always advances its seq before the window completes → adopted,
    /// never taken over (even at a budget-edge re-assoc flush). 0 on boot/gateway-flush/wifi-only
    /// (those use the single-signal `MC_STALE_MS` path anyway) → `max(35s, 0)` = unchanged.
    pub recovery_stale_floor_ms: u64,
    /// #51 return-flap fix: true ONLY on the one-shot BOOT election. A freshly-booted board
    /// must NEVER claim over a DIFFERENT owner already present in the retained MC — it comes
    /// up as a leaf and lets leaf-scan (fast HELLO lock) + the recovery election decide (live
    /// owner → adopt, no flap; dead → take over after the recovery window). Only claims at
    /// boot when the MC is empty or already names THIS board. Gateway-flush keeps `boot=false`
    /// so a running gateway's lowest-id split-brain resolution (#2) is unchanged.
    pub boot: bool,
    /// #146 CLAIM guard: true iff the caller has LATCHED this board out of ownership because it
    /// abdicated on sustained flush failure (`mode.rs` `flush_fail_latch`). When set, the resolver
    /// refuses to (re)claim the crown in ANY arm — including re-grabbing this board's own stale
    /// retained `MC` (the `owner == node_id` self-reclaim that defeated R-DEMOTE in issue #146) —
    /// and leaves the record to freeze so a flush-capable board takes over. Leaf adoption of a live
    /// owner is unaffected. Always false on boot/gateway-flush and for a healthy fleet (a board that
    /// can flush is never latched), so this is a no-op on every path except a proven-incapable owner.
    pub flush_incapable: bool,
    /// #155 channel-drag operator lever: the retained `smol/mesh/channel_hint` value (a decimal
    /// 2.4 GHz channel), or `None` when the topic is absent/empty/garbage. Seeded by
    /// [`mqtt_session`] from the broker each burst. When `Some(h)`, this board's own channel
    /// (`my_channel`) is KNOWN (non-zero) and != `h`, the claim gate refuses to (re)claim the crown
    /// — so the mesh converges onto a crown actually on the hinted channel and the drag heals.
    /// FAIL-OPEN on an unknown own-channel (`my_channel == 0`): a not-yet-learned board claims as
    /// before, so a mesh is never left crownless while a channel is still being learned. `None` ⇒
    /// no gate ⇒ election unchanged. Same claim-guard shape as `flush_incapable` (#146).
    pub channel_hint: Option<u8>,
    // --- outputs (applied to the live role by the caller) ---
    /// True iff I claimed / hold ownership (I am the coexist gateway).
    pub i_am_owner: bool,
    /// The elected owner's id (== my_id when I own it).
    pub owner_id: u8,
    /// #51: true iff the adopted owner was GENUINELY LIVE (fresh seq), false iff it
    /// was dead-but-inside-our-backoff (a deferred takeover). The caller grace-resets
    /// its owner-silence clock ONLY for a genuinely-live owner — a dead-deferred owner
    /// gets no reset, so the next recovery burst fires on cadence (faster failover).
    pub owner_alive: bool,
    /// #155: true iff the CHANNEL-HINT claim gate fired this burst — i.e. we would have claimed /
    /// held the crown but our channel != the operator's `channel_hint`, so we yielded. The caller
    /// uses this on the gateway-flush path to go HELLO-silent on a hint-driven demote (like an
    /// R-DEMOTE abdication), so a sitting crown vacates promptly and leaves re-elect a
    /// hinted-channel board instead of staying pinned to our now-wrong-channel HELLO.
    pub hint_blocked: bool,
}

#[cfg(feature = "wifi")]
impl MeshElect {
    pub fn new(my_id: u8) -> Self {
        Self {
            now_ms: 0,
            seen_owner: 0,
            seen_seq: 0,
            seen_ms: 0,
            my_rssi: -99, // weak default until the first association captures it
            my_channel: 0, // #29: advisory 0 until a frame's rx_control is learned
            recovery: false,
            owner_never_heard: false,
            recovery_stale_floor_ms: 0, // #136: seeded by the caller on a recovery election
            boot: false,
            flush_incapable: false, // #146: seeded by the caller from the flush-fail abdication latch
            channel_hint: None, // #155: seeded by the caller from the retained smol/mesh/channel_hint
            i_am_owner: false,
            owner_id: my_id,
            owner_alive: false,
            hint_blocked: false, // #155: set by the claim gate on a channel-hint yield
        }
    }
}

/// #51: map a board's RSSI-to-AP (dBm) → how long it must WAIT, beyond `RECOVERY_STALE_MS`,
/// before it may take over a dead owner. Stronger signal → shorter wait, so the best-uplink
/// survivor claims the vacated ownership FIRST and publishes a fresh (retained) `MC`; weaker
/// survivors, still inside their (longer) backoff, then observe that LIVE owner and adopt it
/// — the WiFi-strength election JP asked for in #51, node-id only breaking exact-bucket ties.
///
/// The bucket step (`RSSI_BUCKET_STEP_MS`) is deliberately LARGER than the recovery-burst
/// cadence (`REELECT_RETRY_MS`), so a weaker board always has a recovery burst BETWEEN the
/// stronger board's claim and its own claim threshold — it reads the stronger board's retained
/// MC and adopts it, and thus NEVER claims. That is what keeps the RSSI winner STABLE: with no
/// competing claim, the gateway-flush path's lowest-id resolver never fires to undo it. (Two
/// boards in the SAME bucket can still co-claim in one window → resolved deterministically by
/// that lowest-id flush path = the intended node-id tiebreak for equal signal.)
/// No new `MC` wire field: a board only ever compares its OWN rssi against this threshold.
#[cfg(feature = "wifi")]
fn reelect_backoff_ms(rssi: i8, node_id: u8) -> u64 {
    // Bucket by signal strength (typical STA range ≈ -30 strong … -90 weak dBm).
    let bucket: u64 = if rssi >= -65 {
        0
    } else if rssi >= -78 {
        1
    } else {
        2
    };
    // One bucket step per weaker bucket (> the burst cadence, see above) + a small per-id
    // term so same-bucket boards prefer the lower id (final tiebreak; sub-cadence, so it
    // only orders an already-converging same-window co-claim, never separates buckets).
    bucket * RSSI_BUCKET_STEP_MS + (node_id as u64) * 200
}

/// #51 recovery: RSSI backoff step. MUST exceed `REELECT_RETRY_MS` (10 s) so a weaker board
/// gets a recovery burst (→ reads the winner's retained MC → adopts) before its own claim
/// threshold — that's what keeps the stronger board's win stable (no competing claim, so the
/// lowest-id flush resolver never fires to undo it).
#[cfg(feature = "wifi")]
const RSSI_BUCKET_STEP_MS: u64 = 15_000;

/// OTA (#33 Model-A): the ONE retained fleet STAGING topic (`OTA|build|size|sha256|url`)
/// published by `ota_publish.sh stage`. Every board subscribes it as its `latest_version`
/// source + the fetch TARGET — but a staged line NEVER auto-fetches; the board fetches only
/// on its own per-device HA Update `install` command. There is deliberately NO per-id
/// `smol/ota/announce/<id>` act-topic (that path is dropped) — so no publish can trigger a
/// fleet fetch. That structural absence is the #32 canary-discipline closure.
#[cfg(feature = "wifi")]
const OTA_STAGED_TOPIC: &[u8] = b"smol/ota/staged";

/// A retained owner whose `seq` has not advanced for this long is presumed DEAD and
/// may be taken over. The owner re-publishes `MC` (seq++) every gateway flush (~30 s),
/// so 3 missed refreshes with a frozen seq is a safe "owner gone" threshold. Consumed
/// by the [`mqtt_session`] adopt decision (a leaf re-election is what re-reads `MC`
/// after a prolonged HELLO silence, giving the stale check a second sample to fire on).
#[cfg(feature = "wifi")]
const MC_STALE_MS: u64 = 90_000;

/// #51 speed-up: the dead-owner window used ONLY on a LEAF RECOVERY election. It can be far
/// shorter than `MC_STALE_MS` because recovery carries INDEPENDENT corroboration — the leaf
/// only re-elects after `REELECT_SILENCE_MS` of owner-HELLO silence (a live gateway HELLOs
/// every 2 s, so an audible one never reaches here). A takeover thus means the owner is quiet
/// on BOTH the mesh (HELLO) AND the broker (MC seq frozen this long).
/// LOWER BOUND: it MUST stay above the gateway's MC-republish cadence (`RELAY_FLUSH_INTERVAL_MS`
/// ≈ 30 s) — a genuinely-alive gateway's seq is frozen up to one flush interval between flushes,
/// and the seq-advance-resets-`alive` guard only protects us if our window spans a full flush.
/// 35 s = one flush + margin → confidently dead, ~half the old MC_STALE_MS latency. Boot/
/// gateway-flush keep `MC_STALE_MS` (single-signal, no HELLO corroboration → keep the 3× margin).
#[cfg(feature = "wifi")]
const RECOVERY_STALE_MS: u64 = 35_000;

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

/// #155: parse a retained `smol/mesh/channel_hint` payload → the hinted 2.4 GHz channel.
/// A single decimal `u8` (the operator publishes `1`/`6`/`11`); surrounding ASCII whitespace is
/// tolerated. An EMPTY payload (the retain-clear) or any malformed / out-of-range value → `None`
/// (no hint) — so clearing the topic restores the un-hinted election, and a typo (e.g. `99`) can
/// never wedge the mesh onto a channel no board can be on (fail-open). Panic-free (checked parse,
/// no indexing). Accepts only 1..=13 (real 2.4 GHz channels); 0 is the advisory sentinel elsewhere.
#[cfg(feature = "wifi")]
fn parse_channel_hint(payload: &[u8]) -> Option<u8> {
    let ch: u8 = core::str::from_utf8(payload).ok()?.trim().parse().ok()?;
    if (1..=13).contains(&ch) {
        Some(ch)
    } else {
        None
    }
}

/// #21/#48/#55 leaf-relay: extract the leaf id `N` from a `smol/<N><suffix>` topic (the shape
/// the wildcard subscribe delivers), IFF the tail matches `suffix` (e.g. `/config/default_screen`,
/// `/config/led`, `/config/plugins`). Total/panic-free: fixed prefix + exact suffix match + 1–3
/// ASCII-digit parse clamped to u8; anything else → `None`. The topic is broker-supplied, so
/// parse defensively (not just trust the subscribe filter). One helper serves every per-node
/// config key so a new key = one call site, not a new parser.
#[cfg(feature = "wifi")]
fn parse_leaf_config_topic(topic: &[u8], suffix: &[u8]) -> Option<u8> {
    let rest = topic.strip_prefix(b"smol/")?;
    let slash = rest.iter().position(|&b| b == b'/')?;
    let (idb, tail) = rest.split_at(slash);
    if tail != suffix {
        return None;
    }
    if idb.is_empty() || idb.len() > 3 {
        return None;
    }
    let mut val: u16 = 0;
    for &b in idb {
        if !b.is_ascii_digit() {
            return None;
        }
        val = val * 10 + (b - b'0') as u16;
    }
    (val <= 255).then_some(val as u8)
}

/// #40: parse a leaf id out of the wildcard-delivered `smol/<id>/ota/install` topic
/// (the shape `smol/+/ota/install` delivers). Twin of [`parse_leaf_config_topic`] —
/// same defensive parse (broker-supplied topic; 1–3 ASCII digits clamped to u8).
/// `cfg(wifi)`: it is called from the shared `mqtt_session` (a gateway is `espnow`, but
/// the function must still compile in the `wifi`-only build, where it is simply never hit).
#[cfg(feature = "wifi")]
fn parse_leaf_install_topic(topic: &[u8]) -> Option<u8> {
    let rest = topic.strip_prefix(b"smol/")?;
    let slash = rest.iter().position(|&b| b == b'/')?;
    let (idb, tail) = rest.split_at(slash);
    if tail != b"/ota/install" {
        return None;
    }
    if idb.is_empty() || idb.len() > 3 {
        return None;
    }
    let mut val: u16 = 0;
    for &b in idb {
        if !b.is_ascii_digit() {
            return None;
        }
        val = val * 10 + (b - b'0') as u16;
    }
    (val <= 255).then_some(val as u8)
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
pub(crate) const RELAY_FLUSH_BUDGET: Duration = Duration::from_secs(15); // #136: read by the leaf-reelect floor

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
    // #89 Stage 1: painted on each prologue yield so the (wifi-only bench) boot screen
    // shows a LIVE clock through the assoc/DHCP/SNTP sync window instead of a frozen splash.
    render: &mut dyn FnMut(),
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
    let mut elect = MeshElect::new(crate::node_id());
    // wifi-only build has no main-loop OTA/config consume; capture (unused) offers.
    let mut _ota_offer: Option<crate::ota::Announce> = None;
    let mut _config_offer: Option<crate::app::DefaultScreen> = None;
    let mut _install_requested = false;
    let synced = run_ntp_burst(
        &mut controller,
        &mut device,
        rng,
        &mut || false, // wifi-only bench: no #20 abort button wired
        render,
        &mut reached_dhcp,
        crate::node_id(),
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

// ===========================================================================
// #89 Stage 1 — non-blocking NTP prologue substrate (assoc / DHCP / SNTP).
// ===========================================================================
//
// The pre-MQTT prologue of `run_ntp_burst` (WiFi association, DHCP, one SNTP
// exchange) used to be three back-to-back blocking `loop { tick(); iface.poll();
// … }` spins. Each spin idles waiting on a radio/DHCP/UDP round-trip — wall-clock
// the UI thread should have spent rendering. `NtpMachine` turns those three waits
// into ONE resumable state machine polled from the boot path: `poll()` advances
// the current phase, keeps polling while smoltcp reports forward progress, and
// returns `Pending` the moment it stalls (or after `BURST_POLL_BUDGET` of
// continuous progress) so the caller can paint a live clock frame + poll the #20
// abort button between polls.
//
// The MQTT tail (`mqtt_session`) is DELIBERATELY still blocking this stage (that
// is #89 Stage 2) — the machine hands it the live stack and the screen freezes for
// the ≤ `MQTT_SESSION_BUDGET` tail exactly as before. Reverting Stage 1 alone
// restores the old blocking prologue with nothing stranded (no later-stage
// substrate consumer exists yet).
//
// Buffer hoist (F2 precedent — see `OTA_TCP_RX` in `run_ota_fetch`): the smoltcp
// socket storage + per-socket buffers live in module `static mut` so the machine
// can hold `SocketSet<'static>` ACROSS polls. Alias-safe for the same reason the
// OTA fix is: `run_ntp_burst` is boot-only, single-caller, main-thread, and never
// re-entered (periodic flushes / re-elections use `run_mqtt_burst`, not this
// path), so the previous borrow always ends when `run_ntp_burst` returns before
// any next call. `addr_of_mut!` avoids the reference-to-`static mut` lint.

/// #142: association attempts on the single baked `WIFI_NETWORK` before giving up
/// (mesh-only this burst; retry next boot — never a network switch).
#[cfg(feature = "wifi")]
const ASSOC_ATTEMPTS: u8 = 3;

/// #89 ⚠️ HARDWARE-WATCH tuning knob (sibling to the retired `SYNC_REDRAW_MS`): the
/// belt-and-suspenders cap on how long `NtpMachine::poll` may keep polling while
/// smoltcp reports continuous forward progress before it yields a frame anyway. On
/// the NTP path progress is never continuous beyond one round-trip, so this cap
/// effectively never trips here — it earns its keep on the Stage 2/3 transfer paths
/// that reuse this substrate.
#[cfg(feature = "wifi")]
const BURST_POLL_BUDGET: Duration = Duration::from_millis(20);

// --- Hoisted smoltcp stack buffers (F2 precedent; boot-only, single-caller) ---
#[cfg(feature = "wifi")]
static mut NTP_SOCK_STORAGE: [SocketStorage; 4] = [SocketStorage::EMPTY; 4];
#[cfg(feature = "wifi")]
static mut NTP_UDP_RX_META: [udp::PacketMetadata; 4] = [udp::PacketMetadata::EMPTY; 4];
#[cfg(feature = "wifi")]
static mut NTP_UDP_RX_DATA: [u8; 512] = [0; 512];
#[cfg(feature = "wifi")]
static mut NTP_UDP_TX_META: [udp::PacketMetadata; 4] = [udp::PacketMetadata::EMPTY; 4];
#[cfg(feature = "wifi")]
static mut NTP_UDP_TX_DATA: [u8; 512] = [0; 512];
#[cfg(feature = "wifi")]
static mut NTP_TCP_RX: [u8; 512] = [0; 512];
#[cfg(feature = "wifi")]
static mut NTP_TCP_TX: [u8; 512] = [0; 512];
/// DHCP hostname option — must be `'static` because `dhcpv4::Socket<'a>` borrows it
/// for the socket's lifetime and this socket lives `'static` in the hoisted `SocketSet`.
#[cfg(feature = "wifi")]
static NTP_DHCP_OPTS: [DhcpOption<'static>; 1] = [DhcpOption { kind: 12, data: b"smol" }];

/// The resumable prologue phase. `Assoc → Dhcp → Sntp → Done | Failed`.
#[cfg(feature = "wifi")]
enum NtpPhase {
    /// WiFi association: run the (blocking, brief) disconnect/configure/start/connect
    /// FFI once per attempt, then poll `is_connected()` within a per-attempt
    /// `SYNC_BUDGET`. Up to `ASSOC_ATTEMPTS` (#142). All attempts timing out → `Failed`.
    Assoc,
    /// DHCP: poll for a lease within the shared DHCP+SNTP `SYNC_BUDGET`. On a lease,
    /// qualify the node as gateway (N3c) and advance to `Sntp`; timeout → `Failed`.
    Dhcp,
    /// One SNTP exchange within the same shared deadline; a parsed reply → `Done(Some)`,
    /// deadline → `Done(None)` (DHCP already succeeded, so the MQTT tail still runs).
    Sntp,
    /// Prologue complete, DHCP reached: the caller runs the (still-blocking) MQTT tail
    /// on the live stack, then returns this SNTP result.
    Done(Option<u32>),
    /// Assoc or DHCP gave up: the caller returns `None` with NO MQTT tail (mesh-only leaf).
    Failed,
}

/// One `NtpMachine::poll` outcome.
#[cfg(feature = "wifi")]
enum NtpPoll {
    /// Prologue stalled on a round-trip — paint a frame + poll abort, then poll again.
    Pending,
    /// Assoc/DHCP failed: caller returns `None`, skips the MQTT tail (mesh-only leaf).
    Failed,
    /// Reached DHCP; carries the SNTP result. Caller runs the (blocking) MQTT tail on the
    /// machine's live stack, then returns this value.
    ReachedDhcp(Option<u32>),
}

/// #89 Stage 1: the resumable assoc/DHCP/SNTP prologue. Holds the (hoisted) smoltcp
/// stack + phase + timers ACROSS polls; the caller drives `poll()` and renders between
/// yields. Built once per boot burst, dropped at burst end (per-burst freshness — a
/// fresh interface each burst keeps the empty-neighbour-cache behaviour identical to
/// the pre-#89 path).
#[cfg(feature = "wifi")]
struct NtpMachine {
    iface: Interface,
    sockets: SocketSet<'static>,
    dhcp_handle: smoltcp::iface::SocketHandle,
    udp_handle: smoltcp::iface::SocketHandle,
    tcp_handle: smoltcp::iface::SocketHandle,
    phase: NtpPhase,
    /// #142 association attempt counter (0-based).
    attempt: u8,
    /// Whether the current attempt's connect FFI bookend has been issued.
    assoc_configured: bool,
    /// Per-attempt assoc budget, then the shared DHCP+SNTP budget (set on Assoc→Dhcp).
    deadline: Instant,
    /// RNG-seeded ephemeral source port for the SNTP exchange.
    sntp_src_port: u16,
    sntp_bound: bool,
    sntp_sent: bool,
}

#[cfg(feature = "wifi")]
impl NtpMachine {
    /// Build the hoisted smoltcp stack (DHCP + UDP/SNTP + TCP/MQTT sockets over the
    /// module `static mut` buffers) and start in `Assoc`. `sntp_src_port` is the
    /// caller's RNG-seeded ephemeral port for the SNTP request.
    fn new(device: &mut esp_wifi::wifi::WifiDevice, sntp_src_port: u16) -> Self {
        let iface = create_interface(device);

        // SAFETY: F2 precedent — boot-only, single-caller, main-thread, never re-entered
        // (see the module note above), so these `static mut` borrows never alias.
        // Array→slice unsized coercion at the `let` type (NOT `(*ptr)[..]` indexing,
        // which trips the deny-by-default `dangerous_implicit_autorefs`). The referent
        // is a `static`, so the borrow is soundly `'static` and the machine holds the
        // resulting `SocketSet<'static>` across polls.
        let sock_storage: &'static mut [SocketStorage] =
            unsafe { &mut *core::ptr::addr_of_mut!(NTP_SOCK_STORAGE) };
        let udp_rx_meta: &'static mut [udp::PacketMetadata] =
            unsafe { &mut *core::ptr::addr_of_mut!(NTP_UDP_RX_META) };
        let udp_rx_data: &'static mut [u8] =
            unsafe { &mut *core::ptr::addr_of_mut!(NTP_UDP_RX_DATA) };
        let udp_tx_meta: &'static mut [udp::PacketMetadata] =
            unsafe { &mut *core::ptr::addr_of_mut!(NTP_UDP_TX_META) };
        let udp_tx_data: &'static mut [u8] =
            unsafe { &mut *core::ptr::addr_of_mut!(NTP_UDP_TX_DATA) };
        let tcp_rx: &'static mut [u8] =
            unsafe { &mut *core::ptr::addr_of_mut!(NTP_TCP_RX) };
        let tcp_tx: &'static mut [u8] =
            unsafe { &mut *core::ptr::addr_of_mut!(NTP_TCP_TX) };

        let mut sockets = SocketSet::new(sock_storage);

        let mut dhcp_socket = dhcpv4::Socket::new();
        dhcp_socket.set_outgoing_options(&NTP_DHCP_OPTS);
        let dhcp_handle = sockets.add(dhcp_socket);

        let udp_socket = udp::Socket::new(
            udp::PacketBuffer::new(udp_rx_meta, udp_rx_data),
            udp::PacketBuffer::new(udp_tx_meta, udp_tx_data),
        );
        let udp_handle = sockets.add(udp_socket);

        let tcp_socket = tcp::Socket::new(
            tcp::SocketBuffer::new(tcp_rx),
            tcp::SocketBuffer::new(tcp_tx),
        );
        let tcp_handle = sockets.add(tcp_socket);

        Self {
            iface,
            sockets,
            dhcp_handle,
            udp_handle,
            tcp_handle,
            phase: NtpPhase::Assoc,
            attempt: 0,
            assoc_configured: false,
            deadline: Instant::now(), // real deadline set on the first assoc setup
            sntp_src_port,
            sntp_bound: false,
            sntp_sent: false,
        }
    }

    /// Advance the prologue. Keeps polling while smoltcp makes forward progress;
    /// returns `Pending` the instant it stalls (yield to render + poll abort) or after
    /// `BURST_POLL_BUDGET` of continuous progress.
    fn poll(
        &mut self,
        controller: &mut esp_wifi::wifi::WifiController<'static>,
        device: &mut esp_wifi::wifi::WifiDevice<'static>,
    ) -> NtpPoll {
        let poll_start = Instant::now();
        loop {
            let progressed = match self.phase {
                NtpPhase::Assoc => self.step_assoc(controller),
                NtpPhase::Dhcp => self.step_dhcp(device),
                NtpPhase::Sntp => self.step_sntp(device),
                NtpPhase::Done(_) | NtpPhase::Failed => false,
            };
            match self.phase {
                NtpPhase::Done(t) => return NtpPoll::ReachedDhcp(t),
                NtpPhase::Failed => return NtpPoll::Failed,
                _ => {}
            }
            if !progressed || Instant::now() >= poll_start + BURST_POLL_BUDGET {
                return NtpPoll::Pending;
            }
        }
    }

    /// Association step. The disconnect/configure/start/connect FFI (a brief esp-wifi
    /// reconfigure, not pumpable — design §2) runs once per attempt; then we poll
    /// `is_connected()`. Returns whether the phase advanced this step (`false` = still
    /// waiting → yield).
    fn step_assoc(&mut self, controller: &mut esp_wifi::wifi::WifiController<'static>) -> bool {
        let net = &crate::secrets::WIFI_NETWORK;
        if !self.assoc_configured {
            // Drop any prior association before reconfiguring (harmless if not connected).
            let _ = controller.disconnect();
            let ok = controller
                .set_configuration(&Configuration::Client(ClientConfiguration {
                    ssid: net.ssid.into(),
                    password: net.pass.into(),
                    // COEXIST SOAK (#23 PART 1): pin association to ch1.
                    #[cfg(feature = "coexist-soak")]
                    channel: Some(1),
                    ..Default::default()
                }))
                .is_ok()
                && (matches!(controller.is_started(), Ok(true)) || controller.start().is_ok())
                && controller.connect().is_ok();
            if !ok {
                return self.assoc_attempt_failed();
            }
            self.assoc_configured = true;
            self.deadline = Instant::now() + SYNC_BUDGET; // per-attempt assoc budget
            return true;
        }
        if matches!(controller.is_connected(), Ok(true)) {
            log::info!("smol #142: associated to '{}'", net.ssid);
            // Shared DHCP+SNTP budget starts now (mirrors the pre-#89 `deadline` set at
            // the top of the DHCP loop).
            self.deadline = Instant::now() + SYNC_BUDGET;
            self.phase = NtpPhase::Dhcp;
            return true;
        }
        if Instant::now() > self.deadline {
            log::warn!(
                "smol #142: assoc timed out on '{}' — retry next burst (mesh-only leaf)",
                net.ssid
            );
            return self.assoc_attempt_failed();
        }
        false // still waiting on association → yield
    }

    /// Count one failed association attempt; give up (→ `Failed`) after `ASSOC_ATTEMPTS`
    /// (#142: retry the ONE baked network, never switch SSID, no NVS write on failure).
    fn assoc_attempt_failed(&mut self) -> bool {
        self.attempt += 1;
        self.assoc_configured = false; // re-run the connect bookend next attempt
        if self.attempt >= ASSOC_ATTEMPTS {
            log::warn!("smol #142: primary assoc unreachable — mesh-only this burst, retry next");
            self.phase = NtpPhase::Failed;
        }
        true
    }

    /// DHCP step: one `iface.poll()`, apply a lease if it arrived. On a lease, set the
    /// gateway qualifier (N3c: `run_ntp_burst` returns `ReachedDhcp`) and advance to SNTP.
    fn step_dhcp(&mut self, device: &mut esp_wifi::wifi::WifiDevice<'static>) -> bool {
        let changed = matches!(
            self.iface.poll(smoltcp_now(), device, &mut self.sockets),
            smoltcp::iface::PollResult::SocketStateChanged
        );
        let configured = {
            let socket = self.sockets.get_mut::<dhcpv4::Socket>(self.dhcp_handle);
            match socket.poll() {
                Some(dhcpv4::Event::Configured(cfg)) => Some((cfg.address, cfg.router)),
                _ => None,
            }
        };
        if let Some((addr, router)) = configured {
            apply_dhcp(&mut self.iface, addr, router);
            log::info!("smol: DHCP address {}", addr);
            self.phase = NtpPhase::Sntp;
            return true;
        }
        if Instant::now() > self.deadline {
            log::warn!("smol: DHCP timed out");
            self.phase = NtpPhase::Failed;
            return true;
        }
        changed // progress iff smoltcp readiness changed this poll; else yield
    }

    /// SNTP step: bind once, send the NTPv4 request once the socket can send, parse a
    /// reply into Unix seconds. Deadline → `Done(None)` (DHCP already succeeded → MQTT
    /// tail still runs, just no time this burst).
    fn step_sntp(&mut self, device: &mut esp_wifi::wifi::WifiDevice<'static>) -> bool {
        let changed = matches!(
            self.iface.poll(smoltcp_now(), device, &mut self.sockets),
            smoltcp::iface::PollResult::SocketStateChanged
        );
        let socket = self.sockets.get_mut::<udp::Socket>(self.udp_handle);

        // Bind the ephemeral source port once (retry next poll if the stack isn't ready).
        if !self.sntp_bound && socket.bind(self.sntp_src_port).is_ok() {
            self.sntp_bound = true;
        }

        let mut progressed = changed;

        // Send the NTPv4 request once (LI=0, VN=4, Mode=3 → first byte 0x23), latched.
        if self.sntp_bound && !self.sntp_sent && socket.can_send() {
            let mut request = [0u8; 48];
            request[0] = 0x23;
            if socket
                .send_slice(&request, (IpAddress::Ipv4(NTP_SERVER_IP), NTP_PORT))
                .is_ok()
            {
                self.sntp_sent = true;
                progressed = true;
            }
        }

        if socket.can_recv() {
            let mut buf = [0u8; 48];
            if let Ok((len, _from)) = socket.recv_slice(&mut buf) {
                if len >= 48 {
                    // Transmit Timestamp seconds field = bytes 40..44, big-endian, from
                    // the NTP epoch (1900).
                    let ntp_secs = u32::from_be_bytes([buf[40], buf[41], buf[42], buf[43]]);
                    if ntp_secs > NTP_TO_UNIX_OFFSET {
                        self.phase = NtpPhase::Done(Some(ntp_secs - NTP_TO_UNIX_OFFSET));
                        return true;
                    }
                }
            }
        }

        if Instant::now() > self.deadline {
            log::warn!("smol: SNTP timed out");
            self.phase = NtpPhase::Done(None);
            return true;
        }

        progressed
    }
}

/// #100: the broker leg (IPv4, port) to connect THIS burst, resolved at RUNTIME from the NVS
/// net-record (default slot 0 if erased/corrupt). Precedence: a Stage-2 CFG-`B` OVERRIDE wins
/// UNLESS it has been auto-disabled (`broker_fallback`, after repeated CONNACK failures) — then, or
/// when there is no override, the ACTIVE slot's baked broker is used. The baked broker is the
/// un-brickable FLOOR: it follows the associated network (own-VLAN leg rule, a cross-VLAN leg drops
/// CONNACK), so a wrong override always self-heals back to a reachable broker. Panic-free slot index.
#[cfg(feature = "wifi")]
fn active_broker() -> (Ipv4Addr, u16) {
    let cfg = crate::ota::read_net_cfg();
    // A CFG-`B` override wins UNLESS it has been auto-disabled (`broker_fallback`).
    if let Some((ip, port)) = cfg.and_then(|c| c.broker.filter(|_| !c.broker_fallback)) {
        return (Ipv4Addr::new(ip[0], ip[1], ip[2], ip[3]), port);
    }
    let net = &crate::secrets::WIFI_NETWORK;
    (
        Ipv4Addr::new(net.broker_ip[0], net.broker_ip[1], net.broker_ip[2], net.broker_ip[3]),
        net.broker_port,
    )
}

/// #100 Stage 2: consecutive broker-connect failures WHILE a CFG-`B` override is active, counted
/// IN-RAM across bursts (resets on reboot — a fresh boot re-tries the override from scratch). At
/// [`BROKER_FAIL_LIMIT`] the override is auto-disabled (see [`note_broker_connect`]). Single-core,
/// main-thread-only (the flush path), so a plain `static mut` matches the OTA-outcome idiom here.
#[cfg(feature = "espnow")]
static mut BROKER_FAIL_STREAK: u8 = 0;

/// #100 Stage 2: how many consecutive failed CONNACKs on an override broker trigger the self-heal.
#[cfg(feature = "espnow")]
const BROKER_FAIL_LIMIT: u8 = 3;

/// #100 Stage 2 un-brickable broker fallback. Called after each gateway MQTT flush with whether the
/// broker CONNACK'd. It only acts when a CFG-`B` override is ACTIVE (set AND not already fallen
/// back) — the baked broker is the floor, so a baked-broker miss never counts. Called ONLY after the
/// WiFi association already succeeded (see the call site), so a failure here is genuinely a
/// broker/TCP problem, not a WiFi flap. On [`BROKER_FAIL_LIMIT`] consecutive misses it disables the
/// override with ONE NVS write (`broker_fallback = true`, keeping `broker` for DIAG + the CFG-`B`
/// edge-trigger); the next flush uses the slot's baked broker. A success resets the streak. Once
/// fallen back it early-returns (zero further writes — the anti-ping-pong / zero-wear guarantee,
/// mirroring the WiFi assoc fallback). The escape is a NEW CFG-`B` value (which clears the flag).
#[cfg(feature = "espnow")]
pub fn note_broker_connect(connected: bool) {
    let Some(cfg) = crate::ota::read_net_cfg() else {
        return;
    };
    // No ACTIVE override (none set, or already fallen back to baked) → nothing to self-heal from.
    if cfg.broker.is_none() || cfg.broker_fallback {
        unsafe {
            *core::ptr::addr_of_mut!(BROKER_FAIL_STREAK) = 0;
        }
        return;
    }
    if connected {
        unsafe {
            *core::ptr::addr_of_mut!(BROKER_FAIL_STREAK) = 0;
        }
        return;
    }
    let streak = unsafe {
        let p = core::ptr::addr_of_mut!(BROKER_FAIL_STREAK);
        *p = (*p).saturating_add(1);
        *p
    };
    if streak >= BROKER_FAIL_LIMIT {
        // ONE NVS write: disable the override (keep `broker` for DIAG + edge-trigger) → baked broker.
        crate::ota::write_net_cfg(crate::ota::NetCfg { broker_fallback: true, ..cfg });
        unsafe {
            *core::ptr::addr_of_mut!(BROKER_FAIL_STREAK) = 0;
        }
        log::warn!(
            "smol #100: broker override unreachable x{} — disabled, reverting to the slot's baked broker",
            streak
        );
    }
}

/// Shared WiFi -> DHCP -> SNTP burst, reused by both the Phase-2 `wifi`-only
/// build and the Phase-3 `espnow` build. #100: associates on the NVS-selected slot with the
/// un-brickable fallback (see the assoc loop below), drives a `smoltcp` DHCP+UDP stack over
/// `device`, runs one SNTP exchange, and returns the Unix time (seconds) or `None` on any timeout.
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
    // #89 Stage 1: painted on each prologue yield (assoc/DHCP/SNTP stall) so the boot
    // screen shows a LIVE clock through the sync window. UI-agnostic here — the display
    // lives in `main`; net/ just calls back. NOT invoked during the (still-blocking)
    // MQTT tail below, which freezes exactly as before (that is #89 Stage 2).
    render: &mut dyn FnMut(),
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
    // --- #89 Stage 1: resumable assoc → DHCP → SNTP prologue --------------
    // The three blocking waits (assoc, DHCP, SNTP — up to ASSOC_ATTEMPTS × SYNC_BUDGET
    // for assoc, then a shared DHCP+SNTP SYNC_BUDGET) are now one `NtpMachine` polled
    // from here. On each round-trip stall we `render()` a live clock frame + `tick()`
    // (LED + #20 long-press abort), so the boot screen ticks through the whole sync
    // window instead of holding a frozen splash. The machine's hoisted `static mut`
    // stack (F2 precedent) hands off to the still-blocking MQTT tail below.
    //
    // #142 (unchanged): assoc retries the ONE baked network forever, never switches
    // SSID, writes no NVS on failure; all attempts timing out → mesh-only this burst,
    // retry next boot.
    let sntp_src_port = 49152 + (rng.random() % 16384) as u16;
    let mut machine = NtpMachine::new(device, sntp_src_port);
    let synced = loop {
        match machine.poll(controller, device) {
            NtpPoll::Pending => {
                render(); // paint the live clock frame during the round-trip wait
                if tick() {
                    return None; // #20 long-press → unwind the burst (no MQTT tail)
                }
            }
            // Assoc or DHCP gave up → mesh-only leaf this burst; NO MQTT tail (matches
            // the pre-#89 early returns). The next burst/boot retries the primary (#142).
            NtpPoll::Failed => return None,
            NtpPoll::ReachedDhcp(t) => break t,
        }
    };
    // N3c: the machine only yields `ReachedDhcp` on an association + DHCP lease — that
    // alone qualifies the node as a relay GATEWAY (see start()); SNTP is best-effort for
    // TIME, so an SNTP outage can't demote a node with a working LAN uplink.
    *reached_dhcp = true;

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
    let mut _leaf_seen_boot = false; // #40 #1: boot burst is not a gateway relay → never set
    let mut _ntp_gw_own = GwOwnCfg::new(); // #48: boot/NTP burst never captures gateway-own cfg (cfg_cache=None)
    let mut _ntp_reset_req = ResetReq::new(); // #52: boot/NTP burst subscribes no cmd/reset (cfg_cache=None)
    let mut _ntp_scan_req = ScanReq::new(); // #71: boot/NTP burst subscribes no cmd/scan (cfg_cache=None)
    // #89 Stage 1: hand the machine's LIVE stack to the UNCHANGED blocking session —
    // the boot screen freezes for this ≤ MQTT_SESSION_BUDGET tail exactly as before
    // (making the tail cooperative is #89 Stage 2). `tick` still runs (LED + abort),
    // but `render` is not called here, so the display holds its last clock frame.
    let _ = mqtt_session(
        &mut machine.iface,
        device,
        &mut machine.sockets,
        machine.tcp_handle,
        node_id,
        &[],
        mqtt_port,
        batt,
        grid,
        elect,
        ota_offer,
        config_offer,
        &mut _ntp_gw_own,
        &mut _ntp_reset_req,
        install_requested,
        &mut _leaf_seen_boot, // #40 #1: boot burst sees no leaf installs
        &[], // #27: boot NTP+downlink burst publishes no peers (no roster yet)
        &[], // #50: boot burst publishes no live-screen status
        None, // #21: boot burst is not a gateway relay (no leaf-config cache)
        None, // #50b: boot burst republishes no cached leaf status
        &[], // #70/#49: boot burst publishes no own diag
        None, // #70/#49: boot burst republishes no cached diag
        &[], // #71: boot burst publishes no own scan
        None, // #71: boot burst republishes no cached scan
        &mut _ntp_scan_req, // #71: boot burst subscribes no cmd/scan (cfg_cache=None)
        &mut None, // #40: boot burst never relays a leaf OTA
        &mut None, // #40: boot burst has no persistent staged to carry
        &mut None, // #40: boot burst has no relay diag to publish
        &mut None, // #3: boot burst has no relay RX-diag to publish
        &mut None, // #139-followup: boot/NTP burst is never a self-OTA fetch
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

use core::fmt::Write as _;

/// Heap-free scratch buffer for building an MQTT topic / client-id / discovery JSON
/// via `write!`. 320 bytes (bumped from 224 for #33, D6) holds the largest discovery
/// config (~170 B) + the update state JSON with `title` (~140 B), each built + sent
/// sequentially so only one need fit at a time — with headroom for future fields.
#[cfg(feature = "wifi")]
struct MqttScratch {
    // Stays 320: it backs 11 short scratch instances (topics ≤64 B, #33 djson 214 B,
    // sjson 96 B, MC 20 B…) that all fit. Bumping it to 512 would have grown EVERY
    // one of those stack locals in mqtt_session — argus/team-lead's F1-must-not-undo-F2
    // warning. The ONLY payload that exceeded 320 is the #12 discovery JSON (373 B);
    // that one alone moved to a `.bss` static (`MQTT_JSON`/`JsonScratch`), so the
    // mqtt_session frame nets SMALLER than pre-F1 (−320 for json off-stack, +192 for
    // the pkt bump) → the F1 fix cannot reintroduce the F2 stack overflow.
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

/// F1 (oracle): a 512-B scratch for the ONE oversized MQTT payload — the #12 typed
/// discovery JSON (373 B, which overflowed the 320-B [`MqttScratch`] → silent truncate
/// → HA rejected the config → typed entities never appeared). Held in a `.bss` static
/// ([`MQTT_JSON`]), NOT on the mqtt_session stack, so fixing the capacity does NOT grow
/// the stack frame (F1-must-not-reintroduce-F2). Single-caller (one burst at a time)
/// → the `&'static mut` borrow is alias-safe. Same `write!`/`as_bytes` shape as
/// `MqttScratch`; `clear()` resets it between the 3 per-node configs.
#[cfg(feature = "wifi")]
struct JsonScratch {
    buf: [u8; 512],
    len: usize,
}

#[cfg(feature = "wifi")]
impl JsonScratch {
    const fn new() -> Self {
        Self { buf: [0; 512], len: 0 }
    }
    fn clear(&mut self) {
        self.len = 0;
    }
    fn as_bytes(&self) -> &[u8] {
        &self.buf[..self.len]
    }
}

#[cfg(feature = "wifi")]
impl core::fmt::Write for JsonScratch {
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

/// F1: the static backing for [`JsonScratch`] — off the mqtt_session stack (see above).
#[cfg(feature = "wifi")]
static mut MQTT_JSON: JsonScratch = JsonScratch::new();

/// F4 (oracle): the inbound MQTT byte-stream accumulation buffer, 512 B, in a `.bss`
/// static — NOT on the mqtt_session stack. At 256 B it OVERFLOWED on a long-url #33
/// staged announce (`OTA|build|size|sha|url`, url ≤160 B ⇒ packet ~380–410 B): the
/// PUBLISH never fully accumulated, so `parse_packet` never returned it and the
/// announce was silently never read (F1-class, on the CORE OTA path — and it blocks
/// the OTA-proof: the release board can't read the canary's staged announce). 512 also
/// covers the #32 signed 6-field announce (~410 B) → zero rework for that wave. Held in
/// a static (like `MQTT_JSON`) so the +256 does NOT grow the stack frame → cannot
/// reintroduce F2. Single-caller (one burst) → alias-safe.
#[cfg(feature = "wifi")]
static mut MQTT_ACC: [u8; 512] = [0; 512];

/// #21 leaf-relay: max bytes of a relayed keyed-CFG value. Lives here (not `net::mode`) because
/// the gateway FILLS the cache from MQTT in `mqtt_session` (compiled under `wifi`), while the
/// ESP-NOW frame layer that CONSUMES it is `#[cfg(espnow)]` — and `espnow ⊃ wifi`, so a
/// wifi-level type is namable from both with no signature cfg.
///
/// **64** (was 16): the #45 Custom-screen wire (`<count>|<size><align>text;…`, up to 4 segments
/// clipped to 12 chars → ≈ 61 B) is the largest keyed value; screen/led/units/plugins/reboot all
/// stay ≤ 12 B. Sizing the ONE uniform buffer to the largest key reuses the CFG frame verbatim
/// (issue #45: "reuse that frame, don't invent one") rather than a per-key buffer or a second
/// transport. Also removes the old 16-B truncation risk on the STAT uplink (which reuses this).
/// Cost: ~2 KB `.bss` across cfg_cache + stat_cache + the tracker — comfortable on the C3.
#[cfg(feature = "wifi")]
pub const CFG_VALUE_MAX: usize = 64;

/// #56 keyed CFG: the single-ASCII config KEY that follows the 3-digit target id in a
/// `SMOLv1 CFG` frame (`<NNN><KEY><value>`). ONE relay now carries N per-node config
/// channels — `S` = default screen (#21, the only channel #56 ships); #48/#43/#55 add
/// `L` (led) / `U` (units) / `P` (plugins). Defined at the `wifi` tier (like
/// `CFG_VALUE_MAX`, §771) so the gateway FILL path (`mqtt_session`, wifi-only) and the
/// ESP-NOW frame layer that RELAYS/parses it (espnow) both name it with no per-profile cfg.
#[cfg(feature = "wifi")]
pub const CFG_KEY_SCREEN: u8 = b'S';
/// #48 blue-LED mode channel (`status`/`on`/`off`). Per-node retained `smol/<id>/config/led`.
/// (#43/#55/#52 add their keys `U`/`P`/`R` + the `CFG_TARGET_ALL` global-units target here as
/// each feature lands, so the const stays used — no dead_code in the interim.)
#[cfg(feature = "wifi")]
pub const CFG_KEY_LED: u8 = b'L';
/// #43 display-units channel (`<F|C>|<24|12>`). GLOBAL, not per-node: the retained topic is
/// `smol/config/units` (no id). The gateway caches it under the broadcast target
/// [`CFG_TARGET_ALL`] so ONE `SMOLv1 CFG <255>U<val>` frame reaches every leaf.
#[cfg(feature = "wifi")]
pub const CFG_KEY_UNITS: u8 = b'U';
/// #43 broadcast TARGET sentinel for a fleet-global CFG frame. No node ever holds id 255
/// (ids are 1..=254 by convention), so it can't collide with a real per-node target. A leaf
/// applies a CFG frame whose target is its own id OR this sentinel (mode.rs `service()` CFG
/// arm); the gateway caches global configs under `(255, key)` and relays them to all leaves.
#[cfg(feature = "wifi")]
pub const CFG_TARGET_ALL: u8 = 255;
/// #55 plugin-visibility channel (ASCII-hex u16 mask, e.g. `007F`). Per-node retained
/// `smol/<id>/config/plugins`. Bit i (see `app::plugin_bit`) set = that app is shown in the
/// Home menu; a leaf gets it relayed (key `P`), the gateway reads its own directly.
#[cfg(feature = "wifi")]
pub const CFG_KEY_PLUGINS: u8 = b'P';
/// #52 remote-reboot COMMAND (key `R`). Rides the CFG WIRE (`SMOLv1 CFG <id>R`) and IS in
/// `CFG_APPLY_KEYS` (a leaf buffers + applies it) — but is NEVER cached / rebroadcast: a
/// cached reboot = a permanent ~10 s reboot-loop soft-brick. The gateway subscribes the
/// TRANSIENT `smol/<id>/cmd/reset` (retain:false) and fires a ONE-SHOT `<id>R` frame on
/// receipt only (own id → self-reboot). The leaf applies it once, with a boot-debounce.
// allow(dead_code): unlike S/L/U/P, the reboot key is NEVER named in a wifi-tier fill arm — R is
// cache-BYPASS (the #52 anti-reboot-loop rule), so the `/cmd/reset` arm captures into `ResetReq`
// WITHOUT a `cache.set(.., R, ..)`. It's referenced only on espnow (mode.rs CFG_APPLY_KEYS + the
// one-shot drain, main's apply, the net re-export), so a wifi-only build sees it unused. Keeping it
// in the wifi-tier CFG-key family (beside S/L/U/P) reads clearer than cfg(espnow)-gating one key.
#[cfg(feature = "wifi")]
#[allow(dead_code)]
pub const CFG_KEY_REBOOT: u8 = b'R';
/// #45 Custom-screen channel (key `Y`). Per-node retained `smol/<id>/config/custom` = the compose
/// wire `<count>|<size><align>text;…` (entities pre-resolved HA-side; empty = clear). A leaf gets
/// it relayed; the gateway reads its own directly. The largest keyed value (drives CFG_VALUE_MAX).
#[cfg(feature = "wifi")]
pub const CFG_KEY_CUSTOM: u8 = b'Y';

/// #71 on-demand WiFi-scan COMMAND (key `W`). EXACT twin of `R` (#52): rides the CFG WIRE
/// (`SMOLv1 CFG <id>W`), IS in `CFG_APPLY_KEYS` (a node buffers + applies it), but is NEVER
/// cached / rebroadcast — a cached/periodic scan would take the single radio off the mesh
/// channel every ~10 s (the exact coexist hazard #71 forbids). The gateway subscribes the
/// TRANSIENT `smol/<id>/cmd/scan` (retain:false) and fires a ONE-SHOT `<id>W` frame on receipt
/// (own id → self-scan via its own CfgTracker). Applying `W` runs ONE WiFi AP scan → the top APs
/// are published to `smol/<id>/scan`. Same cache-BYPASS + wifi-tier-family rationale as `R`.
#[cfg(feature = "wifi")]
#[allow(dead_code)]
pub const CFG_KEY_SCAN: u8 = b'W';
/// #100 network-switch CONFIG (key `N`) = the active WiFi-slot index (`0`/`1`). RETAINED/CACHED
/// STATE (relayed like S/L/U/P/Y, NOT a one-shot command like R/W) — a node applies it by writing
/// the NVS net-record + rebooting into the slot, EDGE-triggered on a change of the commanded slot
/// (re-reading the same retained value is a no-op → never a reboot-loop). Per-node
/// `smol/<id>/config/net` or fleet-wide `smol/config/net` (target 255). Value = one ASCII digit.
#[cfg(feature = "wifi")]
pub const CFG_KEY_NET: u8 = b'N';
/// #100 Stage 2 broker-override CONFIG (key `B`) = the MQTT broker leg `"a.b.c.d"` or `"a.b.c.d:port"`
/// (RFC1918-gated, IP-only v1; empty = clear back to the slot's baked broker). RETAINED/CACHED STATE
/// (relayed like `N`). A node applies it by writing the NVS net-record + rebooting; EDGE-triggered on
/// a change of the COMMANDED broker (a re-read is a no-op → never a reboot-loop, even after the CONNACK
/// fallback disables the override). Per-node `smol/<id>/config/broker` or fleet-wide `smol/config/broker`.
#[cfg(feature = "wifi")]
pub const CFG_KEY_BROKER: u8 = b'B';
/// #100 Stage 3 OTA-host-override CONFIG (key `O`) = one extra RFC1918 image host `"a.b.c.d"` appended
/// to the fetch allowlist (empty = clear). RETAINED/CACHED STATE (relayed like `N`). Applied by writing
/// the NVS net-record — NO reboot (the allowlist is read at fetch/gate time). EDGE-triggered on a change.
/// Per-node `smol/<id>/config/ota_host` or fleet-wide `smol/config/ota_host`.
#[cfg(feature = "wifi")]
pub const CFG_KEY_OTA: u8 = b'O';

/// #72 IO/component registry CONFIG (key `G`) = the node's whole pin-map descriptor:
/// `;`-separated `<pin><kind>` tokens (e.g. `0L;7B;10R`), ≤ `CFG_VALUE_MAX`. RETAINED /
/// CACHED (relayed like S/L/U/P/Y/N, not a one-shot command). Per-node
/// `smol/<id>/config/io`. Applied by (re)binding the free GPIOs via
/// `crate::io::apply_wire`, EDGE-triggered on a CHANGE of the map (a re-read of the same
/// retained value is a no-op). Writes NO NVS (zero flash wear / sector risk — the nvs
/// partition is full); survives reboot purely via the gateway's ~10 s config re-relay.
// allow(dead_code): named in `CFG_APPLY_KEYS` (espnow) unconditionally so a G slot exists
// for the relay, and in the `io`-gated fill/apply plumbing — but NOT in any wifi-tier fill
// arm, so a wifi-only (no-espnow, no-io) build sees it unused. Same rationale as R/W.
#[cfg(feature = "wifi")]
#[allow(dead_code)]
pub const CFG_KEY_IO: u8 = b'G';

/// #72 IO output CONTROL (key `g`, lowercase — distinct from the `G` config map) = the
/// node's output STATES: `;`-separated `<pin>=<0|1>` (e.g. `0=1;10=0`), ≤ `CFG_VALUE_MAX`.
/// RETAINED / CACHED (relayed like G), NOT a command — a lamp/relay holds its commanded
/// level across reboot / relay-loss (re-asserted from the retained value after a re-relay
/// or a `G` re-bind). Applied by driving each named OUTPUT slot via `crate::io::apply_set`
/// (no-op on an unbound / input slot). Per-node `smol/<id>/io/set`. Writes NO NVS.
/// Same allow(dead_code) rationale as `G` (unused in a wifi-only build).
#[cfg(feature = "wifi")]
#[allow(dead_code)]
pub const CFG_KEY_IO_SET: u8 = b'g';

/// #48 (GwOwnCfg — approved arch): the GATEWAY's OWN per-node configs read from its own MQTT
/// topics this burst. A leaf gets these RELAYED (→ its `CfgTracker`); the gateway reads them
/// DIRECTLY. Bundled into ONE `run_mqtt_burst`/`mqtt_session` out-param (net +1, not +N) — after
/// the burst `service()` injects each present value into the gateway's OWN (otherwise-idle)
/// `CfgTracker`, so `main`'s `take_cfg_offer(key)` applies it on the EXACT same path as a leaf's
/// relayed config (a node is gateway XOR leaf → the one tracker has a single feeder). Screen stays
/// on its own `config_offer` path (untouched). #43/#55 add `units`/`plugins` fields as they land.
#[cfg(feature = "wifi")]
#[derive(Clone, Copy)]
// The fields are READ only on espnow (mode.rs gateway flush injects them into the CfgTracker);
// a wifi-only build FILLS them in mqtt_session but has no RadioManager to read them back, so they
// are write-only there → allow(dead_code) keeps the `-D warnings` clippy gate green in BOTH
// configs (same cross-profile rationale as CfgCache above).
#[allow(dead_code)]
pub struct GwOwnCfg {
    /// The gateway's own `smol/<id>/config/led` value `(buf, len)`, or `None` if absent this burst.
    pub led: Option<([u8; CFG_VALUE_MAX], usize)>,
    /// #43 the GLOBAL `smol/config/units` value `(buf, len)`, or `None` if absent this burst.
    /// The gateway applies its own display units directly (it also relays them to leaves under
    /// the broadcast target); captured here so `service()` self-applies via the same path.
    pub units: Option<([u8; CFG_VALUE_MAX], usize)>,
    /// #55 the gateway's own `smol/<id>/config/plugins` value `(buf, len)`, or `None` if absent.
    pub plugins: Option<([u8; CFG_VALUE_MAX], usize)>,
    /// #45 the gateway's own `smol/<id>/config/custom` value `(buf, len)`, or `None` if absent.
    pub custom: Option<([u8; CFG_VALUE_MAX], usize)>,
    /// #100 the gateway's own `smol/<id>/config/net` (or global `smol/config/net`) active-slot
    /// index value `(buf, len)`, or `None` if absent this burst.
    pub net: Option<([u8; CFG_VALUE_MAX], usize)>,
    /// #100 Stage 2 the gateway's own `smol/<id>/config/broker` (or global `smol/config/broker`)
    /// broker-leg override value `(buf, len)`, or `None` if absent this burst.
    pub broker: Option<([u8; CFG_VALUE_MAX], usize)>,
    /// #100 Stage 3 the gateway's own `smol/<id>/config/ota_host` (or global `smol/config/ota_host`)
    /// OTA-host override value `(buf, len)`, or `None` if absent this burst.
    pub ota: Option<([u8; CFG_VALUE_MAX], usize)>,
    /// #72 the gateway's own `smol/<id>/config/io` pin-map value `(buf, len)`, or `None` if
    /// absent this burst. `io`-gated so a non-io build's struct is byte-unchanged.
    #[cfg(feature = "io")]
    pub io: Option<([u8; CFG_VALUE_MAX], usize)>,
    /// #72 the gateway's own `smol/<id>/io/set` output-states value `(buf, len)`, or `None`.
    #[cfg(feature = "io")]
    pub io_set: Option<([u8; CFG_VALUE_MAX], usize)>,
}

#[cfg(feature = "wifi")]
impl GwOwnCfg {
    pub const fn new() -> Self {
        Self {
            led: None,
            units: None,
            plugins: None,
            custom: None,
            net: None,
            broker: None,
            ota: None,
            #[cfg(feature = "io")]
            io: None,
            #[cfg(feature = "io")]
            io_set: None,
        }
    }
    /// Pack a payload into the `(buf, len)` a field holds (truncated to `CFG_VALUE_MAX`), so the
    /// mqtt-drain arms stay one-liners: `gw_own.led = Some(GwOwnCfg::val(payload));`.
    pub fn val(value: &[u8]) -> ([u8; CFG_VALUE_MAX], usize) {
        let mut b = [0u8; CFG_VALUE_MAX];
        let n = value.len().min(CFG_VALUE_MAX);
        b[..n].copy_from_slice(&value[..n]);
        (b, n)
    }
}

/// #52 how many distinct leaf reboot targets one burst can queue. A reset is TRANSIENT +
/// re-pressable, so a full queue just drops extras (the user re-presses) — no soft state lost.
#[cfg(feature = "wifi")]
pub const RESET_REQ_MAX: usize = 8;

/// #52 remote-reboot capture — the reset COMMANDS seen on the TRANSIENT `smol/+/cmd/reset` topics
/// this burst. NOT a config: NEVER cached / rebroadcast (a cached reboot = a permanent ~10 s
/// reboot-loop soft-brick). Bundled into ONE `mqtt_session`/`run_mqtt_burst` out-param (like
/// `GwOwnCfg`). After the burst, `service()` fires a ONE-SHOT `broadcast_config(id, R, "")` per
/// leaf target (direct ESP-NOW, bypassing `cfg_cache`) and injects R into its OWN `CfgTracker`
/// if `own` — so `main`'s `take_cfg_offer(R)` self-reboots on the SAME boot-debounced path as a
/// leaf. `#[allow(dead_code)]`: read only on espnow (mode.rs), write-only on a wifi-only build.
#[cfg(feature = "wifi")]
#[derive(Clone, Copy)]
#[allow(dead_code)]
pub struct ResetReq {
    targets: [u8; RESET_REQ_MAX],
    n: usize,
    own: bool,
}

#[cfg(feature = "wifi")]
#[allow(dead_code)]
impl ResetReq {
    pub const fn new() -> Self {
        Self { targets: [0; RESET_REQ_MAX], n: 0, own: false }
    }
    /// Queue a leaf id for a one-shot reboot relay (deduped; dropped if full — re-pressable).
    pub fn push_leaf(&mut self, id: u8) {
        for i in 0..self.n {
            if self.targets[i] == id {
                return;
            }
        }
        if self.n < RESET_REQ_MAX {
            self.targets[self.n] = id;
            self.n += 1;
        }
    }
    /// Mark that THIS node's own `cmd/reset` fired this burst → self-reboot after the burst.
    pub fn set_own(&mut self) {
        self.own = true;
    }
    pub fn own(&self) -> bool {
        self.own
    }
    /// The queued leaf reboot targets (to relay one-shot; NEVER cached).
    pub fn targets(&self) -> &[u8] {
        &self.targets[..self.n]
    }
}

/// #71 on-demand WiFi-scan capture — the scan COMMANDS seen on the TRANSIENT `smol/+/cmd/scan`
/// topics this burst. EXACT twin of [`ResetReq`] (a target queue + own flag): NEVER cached (a
/// cached scan = a periodic off-channel excursion, the #71 coexist hazard). After the burst
/// `service()` fires a ONE-SHOT `broadcast_config(id, W, "")` per leaf target + injects `W` into
/// its OWN `CfgTracker` if `own`, so `main`'s `take_cfg_offer(W)` runs the scan on the same path
/// for a leaf or the gateway. `#[allow(dead_code)]`: read only on espnow, write-only on wifi-only.
#[cfg(feature = "wifi")]
#[derive(Clone, Copy)]
#[allow(dead_code)]
pub struct ScanReq {
    targets: [u8; RESET_REQ_MAX],
    n: usize,
    own: bool,
}

#[cfg(feature = "wifi")]
#[allow(dead_code)]
impl ScanReq {
    pub const fn new() -> Self {
        Self { targets: [0; RESET_REQ_MAX], n: 0, own: false }
    }
    /// Queue a leaf id for a one-shot scan relay (deduped; dropped if full — re-triggerable).
    pub fn push_leaf(&mut self, id: u8) {
        for i in 0..self.n {
            if self.targets[i] == id {
                return;
            }
        }
        if self.n < RESET_REQ_MAX {
            self.targets[self.n] = id;
            self.n += 1;
        }
    }
    /// Mark that THIS node's own `cmd/scan` fired this burst → self-scan after the burst.
    pub fn set_own(&mut self) {
        self.own = true;
    }
    pub fn own(&self) -> bool {
        self.own
    }
    /// The queued leaf scan targets (to relay one-shot; NEVER cached).
    pub fn targets(&self) -> &[u8] {
        &self.targets[..self.n]
    }
}

/// #40 #3: one relay attempt's diagnostic snapshot — gateway-side RX evidence PLUS the leaf's
/// self-reported OTA state (captured from its `LDBG` beacon during the relay). Published to
/// retained `smol/<leaf>/ota/relaydiag`. Defined at the `wifi` level (not `ota_mesh`/`mode`,
/// both espnow-only) so this `run_mqtt_burst` publish path names it in the wifi-only profile too.
/// `leaf_verdict == 255` ⇒ no `LDBG` captured (old leaf fw / leaf off-air during the relay).
/// Together they name a `rx>0 otan=0` relay-failed: leaf_heard=0 → OTAM TX not landing on the
/// leaf; verdict 2-6 → `on_meta` rejected (which gate); verdict=1 & leaf_sent=0 → armed but never
/// NAK'd (servicing); leaf_sent>0 & otan_valid=0 → leaf NAK'd but the gateway never heard it.
#[cfg(feature = "wifi")]
#[derive(Clone, Copy)]
pub struct RelayDiag {
    pub leaf_id: u8,
    pub rx_any: u16,
    pub otan_valid: u16,
    pub last_wb: u16,
    pub total: u16,
    pub leaf_heard: u16,
    pub leaf_verdict: u8,
    pub leaf_sent: u16,
    /// #3b TX-diag: OTAM broadcast sends ATTEMPTED / that returned Ok (queued + TX-callback ok).
    /// `otam_ok=0` while `otam_tx>0` ⇒ the send itself fails (peer-table / post-fetch ESP-NOW TX
    /// state) → the announce never egresses (explains leaf H0 with the gateway on-channel).
    /// `otam_ok>0` while leaf stays H0 ⇒ frame egresses but the leaf's RX drops it (deeper).
    pub otam_tx: u16,
    pub otam_ok: u16,
    /// #3b CHANNEL-diag: iterations spent waiting for the WiFi STA to release the PHY after the
    /// fetch, before pinning ch6. `settle>0` ⇒ the STA WAS still holding the AP channel post-fetch
    /// (confirms the OTAM was egressing off-channel → the leaf H0 cause); `settle=0` ⇒ STA already
    /// down, so a persistent H0 is NOT the channel (→ leaf RX-filter, instrument the leaf next).
    pub settle: u16,
    /// #3b LEAF-CHANNEL: the leaf's `current_channel()` from its captured LDBG (0=scanning/unlocked,
    /// else the locked channel). Splits the settle=0 H0 fork: leaf_ch=6 ⇒ leaf on ch6 yet no OTAM
    /// (RX issue); leaf_ch≠6 ⇒ leaf drifted off ch6 during the gateway's mesh-deaf fetch window.
    pub leaf_ch: u8,
}

#[cfg(feature = "wifi")]
const CFG_CACHE_CAP: usize = 16;

/// #68 F6: a cached leaf STAT older than this (ms since last heard) is treated as STALE — its
/// `smol/<id>/status` republish is skipped (no ghost) and its MAC no longer resolves a relay arm.
/// ~4.5× the 10 s STAT cadence: a leaf that missed several STATs is genuinely gone, not just laggy.
#[cfg(feature = "wifi")]
pub const STAT_FRESH_MS: u64 = 45_000;

/// #70/#49 F6 (diag twin): a cached node DIAG older than this is STALE — its `smol/<id>/diag`
/// republish is skipped so an off-air node ages out (no ghost). Sized off the SLOW ~60 s DIAG
/// broadcast cadence (diag is slow-moving, kept low-airtime), NOT the 10 s STAT cadence: at ~2.5×
/// the beat a node that missed 2 diags is gone. MUST exceed the diag cadence or a live node's
/// record would flicker stale between broadcasts (the STAT gate's 45 s would wrongly drop it).
#[cfg(feature = "wifi")]
pub const DIAG_FRESH_MS: u64 = 150_000;

/// #21 leaf-relay: the GATEWAY's per-leaf default-screen cache. Filled from the
/// retained wildcard `smol/+/config/default_screen` during a flush; re-broadcast as
/// `SMOLv1 CFG` frames on the ~10 s cadence (mode.rs `broadcast_cached_configs`) so
/// credential-less leaves converge on their dashboard-set screen — and a (re)joined
/// leaf still gets its config without HA re-publishing. Bounded `.bss`, no heap.
#[cfg(feature = "wifi")]
pub struct CfgCache {
    ids: [u8; CFG_CACHE_CAP],
    /// #56 keyed CFG: the config KEY (`S`/`L`/`U`/`P`) each entry belongs to. Upsert is
    /// now on the COMPOSITE (id, key) so one leaf can hold N per-channel configs at once,
    /// each relayed as its own `SMOLv1 CFG <NNN><KEY><value>` frame. #56 fills only `S`
    /// (from `default_screen`); the column is inert for the single-channel `stat_cache`
    /// reuse (it always upserts under one fixed key → identical id-keyed behaviour).
    keys: [u8; CFG_CACHE_CAP],
    vals: [[u8; CFG_VALUE_MAX]; CFG_CACHE_CAP],
    lens: [u8; CFG_CACHE_CAP],
    /// #68 F6: last-heard timestamp (now_ms) per entry. Gates the stat republish on freshness
    /// (a leaf that goes off-air STOPS refreshing its retained smol/<id>/status → HA sees it go
    /// stale instead of a perpetually-fresh GHOST — the ghost that masked id9's floor-wipe + faked
    /// id8-alive all demo). Also bounds the `mac_for` fallback to recently-heard leaves.
    last_ms: [u64; CFG_CACHE_CAP],
    /// #68: the src MAC the entry was last heard from. Lets the relay arm resolve a STAT-heard
    /// leaf's MAC even after the volatile 16-slot LRU roster evicts it (roster-admission robustness
    /// — "any STAT-heard leaf stays mac_for_id-resolvable"). Only meaningful for stat_cache (uplink);
    /// cfg_cache (downlink configs) passes a zero MAC + is never mac-queried.
    macs: [[u8; 6]; CFG_CACHE_CAP],
    count: usize,
}

#[cfg(feature = "wifi")]
impl CfgCache {
    // `new`/`count`/`entry` are called only by the espnow gateway (RadioManager +
    // broadcast_cached_configs); in a wifi-only build they're unused (the RadioManager
    // doesn't exist) → allow dead_code so the clippy gate stays clean in BOTH configs.
    #[allow(dead_code)]
    pub const fn new() -> Self {
        Self {
            ids: [0; CFG_CACHE_CAP],
            keys: [0; CFG_CACHE_CAP],
            vals: [[0; CFG_VALUE_MAX]; CFG_CACHE_CAP],
            lens: [0; CFG_CACHE_CAP],
            last_ms: [0; CFG_CACHE_CAP],
            macs: [[0; 6]; CFG_CACHE_CAP],
            count: 0,
        }
    }

    /// #56: upsert a leaf's config value under its channel `key` (truncated to
    /// `CFG_VALUE_MAX`). Match/insert is on the COMPOSITE (id, key) so one leaf holds N
    /// keyed configs simultaneously — a `key` change never clobbers a different channel.
    /// A full cache drops the entry and logs it (no silent cap). Value bytes are opaque
    /// here — the gateway RELAYS them verbatim; the leaf's per-key dispatch validates
    /// (screen → `parse_default_screen`). #68 F6: `mac`/`now` stamp the entry for the
    /// stat-cache reuse (freshness gate + MAC-resolvable relay); the downlink cfg_cache
    /// passes a zero MAC and is never mac-queried.
    pub fn set(&mut self, id: u8, key: u8, value: &[u8], mac: [u8; 6], now: u64) {
        let n = value.len().min(CFG_VALUE_MAX);
        for i in 0..self.count {
            if self.ids[i] == id && self.keys[i] == key {
                self.vals[i][..n].copy_from_slice(&value[..n]);
                self.lens[i] = n as u8;
                self.last_ms[i] = now; // #68 F6: freshen
                self.macs[i] = mac;
                return;
            }
        }
        if self.count < CFG_CACHE_CAP {
            let i = self.count;
            self.ids[i] = id;
            self.keys[i] = key;
            self.vals[i][..n].copy_from_slice(&value[..n]);
            self.lens[i] = n as u8;
            self.last_ms[i] = now;
            self.macs[i] = mac;
            self.count += 1;
        } else {
            log::warn!(
                "smol #21/#56: cfg cache full ({}) — dropping id{} key '{}'",
                CFG_CACHE_CAP,
                id,
                key as char
            );
        }
    }

    /// Number of cached leaf configs.
    #[allow(dead_code)]
    pub fn count(&self) -> usize {
        self.count
    }

    /// The `i`-th cached entry as `(leaf_id, key, value_bytes)`, or `None` if out of range.
    /// #56: `key` is the config channel (`S`/…); the `stat_cache` reuse ignores it.
    #[allow(dead_code)]
    pub fn entry(&self, i: usize) -> Option<(u8, u8, &[u8])> {
        if i < self.count {
            let n = self.lens[i] as usize;
            Some((self.ids[i], self.keys[i], &self.vals[i][..n]))
        } else {
            None
        }
    }

    /// #68 F6: the `i`-th entry, but ONLY if it was heard within `ttl` ms of `now`. The stat
    /// republish uses this so a leaf that stopped transmitting stops refreshing its retained
    /// status → HA sees it go stale instead of a perpetually-fresh ghost.
    #[allow(dead_code)]
    pub fn entry_fresh(&self, i: usize, now: u64, ttl: u64) -> Option<(u8, &[u8])> {
        if i < self.count && now.saturating_sub(self.last_ms[i]) <= ttl {
            let n = self.lens[i] as usize;
            Some((self.ids[i], &self.vals[i][..n]))
        } else {
            None
        }
    }

    /// #68: the MAC last heard for `id`, IFF the entry is fresh (within `ttl`). Lets the relay
    /// arm resolve a recently-STAT-heard leaf's MAC even after the LRU roster evicts it — so a
    /// STAT-audible-but-roster-dropped leaf is still armable (vs the silent mac-unknown no-arm).
    #[allow(dead_code)]
    pub fn mac_for(&self, id: u8, now: u64, ttl: u64) -> Option<[u8; 6]> {
        for i in 0..self.count {
            if self.ids[i] == id && now.saturating_sub(self.last_ms[i]) <= ttl {
                return Some(self.macs[i]);
            }
        }
        None
    }
}

/// #70/#71 observability: max bytes of a relayed DIAG or SCAN record value. Larger than
/// `CFG_VALUE_MAX` (16, sized for a screen string) because a diag/scan record is a multi-field
/// line (~130 B) — but still well under the ~250 B ESP-NOW frame budget once the 12 B frame
/// prefix + 3 B id are added. #74 wave-2 folds ~7 more keys onto the DIAG record (led/rtt/rx/tx/
/// tage/tsrc/loss); stage-2 adds the ~24 B `cfg=` applied-config string (config-drift). 232 — the
/// ESP-NOW frame is then 12 (prefix) + 3 (id) + 232 = 247 B, still under the ~250 B ceiling. This
/// bounds ONLY relayed LEAF records (the gateway self-publishes its own full record via MQTT); the
/// ~24 B headroom absorbs long-uptime counter growth (up/rx/tx) so `cfg=` (record tail) survives.
#[cfg(feature = "wifi")]
pub const RELAY_VALUE_MAX: usize = 232;

#[cfg(feature = "wifi")]
const RELAY_CACHE_CAP: usize = 12;

/// #70/#71: the GATEWAY's per-leaf cache of a relayed observability record (DIAG or SCAN). A
/// leaf has no MQTT, so it broadcasts its record over ESP-NOW; the gateway caches the most
/// recent per leaf id and republishes it RETAINED on each flush (`smol/<leaf>/diag`|`/scan`).
/// Twin of [`CfgCache`] but id-keyed only (no config-key / MAC columns — MAC resolution stays
/// with `stat_cache`) and a bigger value buffer. Bounded `.bss`, no heap; instantiated twice
/// (diag + scan). #68 F6 freshness (`entry_fresh`) gates the republish so an off-air leaf's
/// retained record ages out instead of ghosting.
#[cfg(feature = "wifi")]
pub struct RelayCache {
    ids: [u8; RELAY_CACHE_CAP],
    vals: [[u8; RELAY_VALUE_MAX]; RELAY_CACHE_CAP],
    lens: [u16; RELAY_CACHE_CAP],
    last_ms: [u64; RELAY_CACHE_CAP],
    count: usize,
}

#[cfg(feature = "wifi")]
impl RelayCache {
    // Like `CfgCache`, these are called only by the espnow gateway (`RadioManager`); a
    // wifi-only build (no `RadioManager`) leaves `new`/`set`/`count` unused → allow dead_code
    // so the clippy gate stays clean in every profile.
    #[allow(dead_code)]
    pub const fn new() -> Self {
        Self {
            ids: [0; RELAY_CACHE_CAP],
            vals: [[0; RELAY_VALUE_MAX]; RELAY_CACHE_CAP],
            lens: [0; RELAY_CACHE_CAP],
            last_ms: [0; RELAY_CACHE_CAP],
            count: 0,
        }
    }

    /// Upsert leaf `id`'s record (truncated to `RELAY_VALUE_MAX`), stamping `now` for the F6
    /// freshness gate. A full cache drops the entry and logs it (no silent cap).
    #[allow(dead_code)]
    pub fn set(&mut self, id: u8, value: &[u8], now: u64) {
        let n = value.len().min(RELAY_VALUE_MAX);
        for i in 0..self.count {
            if self.ids[i] == id {
                self.vals[i][..n].copy_from_slice(&value[..n]);
                self.lens[i] = n as u16;
                self.last_ms[i] = now;
                return;
            }
        }
        if self.count < RELAY_CACHE_CAP {
            let i = self.count;
            self.ids[i] = id;
            self.vals[i][..n].copy_from_slice(&value[..n]);
            self.lens[i] = n as u16;
            self.last_ms[i] = now;
            self.count += 1;
        } else {
            log::warn!(
                "smol #70/#71: relay cache full ({}) — dropping id{}",
                RELAY_CACHE_CAP,
                id
            );
        }
    }

    /// Number of cached leaf records.
    #[allow(dead_code)]
    pub fn count(&self) -> usize {
        self.count
    }

    /// The `i`-th entry as `(leaf_id, value)`, but ONLY if heard within `ttl` ms of `now`
    /// (#68 F6 freshness gate). Off-air leaves age out instead of ghosting a retained record.
    #[allow(dead_code)]
    pub fn entry_fresh(&self, i: usize, now: u64, ttl: u64) -> Option<(u8, &[u8])> {
        if i < self.count && now.saturating_sub(self.last_ms[i]) <= ttl {
            let n = self.lens[i] as usize;
            Some((self.ids[i], &self.vals[i][..n]))
        } else {
            None
        }
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

/// #147 self-fetch failure POINT — the exact stage a failed `run_ota_fetch` died at, carried as
/// the 5th field of the `(chunk_k, chunk_n, retries, stalls, where)` self-fail record and rendered
/// into the retained `smol/<id>/ota/diag` payload (`… at=<label>`). Release images are serial-
/// silent, so this is the ONLY fleet-visible signal of WHERE a self-fetch died: the #139 record
/// showed how FAR the download got but not WHICH stage wedged (a chunk-2 handshake wedge and a
/// mid-body stall both surfaced as `chunk=1/N retry=0 stall=0`). Defined once here so the espnow
/// fetch loop and the wifi diag formatter agree on the codes.
#[cfg(feature = "wifi")]
mod ota_fail {
    pub const NONE: u32 = 0; // no point recorded (should not surface on a real failure)
    pub const ASSOC: u32 = 1; // WiFi association timed out (pre-download)
    pub const DHCP: u32 = 2; // DHCP lease timed out (pre-download)
    pub const SLOT: u32 = 3; // inactive OTA slot would not open (pre-download)
    pub const CONNECT: u32 = 4; // smoltcp connect() returned Err on the (reused) socket
    pub const HANDSHAKE: u32 = 5; // connect() ok but the TCP handshake never completed in-window
    pub const SEND: u32 = 6; // the HTTP GET/Range request could not be enqueued
    pub const STATUS: u32 = 7; // bad HTTP status / Content-Length on a chunk
    pub const FALLBACK: u32 = 8; // 200 full-body fallback died mid-stream (non-resumable)
    pub const STALL: u32 = 9; // consecutive zero-progress attempts exhausted
    pub const DEADLINE: u32 = 10; // global OTA_FETCH_BUDGET elapsed mid-download
    pub const VERIFY: u32 = 11; // download completed but the size/SHA-256/ed25519 gate rejected it
    pub const RECYCLE: u32 = 12; // the socket never returned to a connectable state between chunks

    /// Short, stable label for the retained diag payload (kept terse — the MQTT packet is capped).
    pub fn label(fp: u32) -> &'static str {
        match fp {
            ASSOC => "assoc",
            DHCP => "dhcp",
            SLOT => "slot",
            CONNECT => "connect",
            HANDSHAKE => "handshake",
            SEND => "send",
            STATUS => "status",
            FALLBACK => "fallback-trunc",
            STALL => "stall",
            DEADLINE => "deadline",
            VERIFY => "verify",
            RECYCLE => "recycle",
            NONE => "none",
            _ => "?",
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
    // #48 GwOwnCfg: filled with the GATEWAY's OWN keyed configs (led, +units/plugins later) read
    // from its own MQTT topics this burst; `service()` injects them into the gateway's CfgTracker.
    gw_own: &mut GwOwnCfg,
    // #52 remote reboot: filled with the reset COMMANDS seen on the transient `smol/+/cmd/reset`
    // topics this burst; `service()` fires a one-shot `<id>R` relay per target (NEVER cached) +
    // self-reboots if own. Separate from `gw_own` because R is a COMMAND, not a cached config.
    reset_req: &mut ResetReq,
    // #33 HA Update entity: set true iff a retained `install` command is present for this
    // board (the native Install button) — the caller AND-gates the fetch on it.
    install_requested: &mut bool,
    // #40 #1: set true iff a retained `smol/<leaf>/ota/install` for ANOTHER node is SEEN this
    // burst (install-seen, independent of whether it armed — arming needs the cached staged
    // image). The gateway flush latches `leaf_ota_pending` on this so its OWN self-OTA is
    // suppressed the moment a leaf install exists, closing the self-OTA-first race.
    leaf_install_seen: &mut bool,
    // #27: this node's serialized roster (`PEERS|…`); published retained to
    // `smol/<node_id>/peers` after the telemetry loop iff non-empty.
    peers: &[u8],
    // #50: this node's live `STAT|<screen>:<page>` (from `App::live_screen`) → retained
    // `smol/<node_id>/status`. Empty ⇒ skipped. (Built once; carries build#/installed for
    // #40 Tier-2 later.)
    status: &[u8],
    // #21 leaf-relay: `Some` on a GATEWAY flush → subscribe the wildcard
    // `smol/+/config/default_screen` and cache every OTHER leaf's value here for the
    // ESP-NOW relay. `None` on boot/leaf/election bursts (no relay duty).
    mut cfg_cache: Option<&mut CfgCache>,
    // #50b leaf-status uplink: `Some` on a GATEWAY flush → after publishing THIS node's
    // own status, republish each CACHED leaf status as retained `smol/<leaf>/status`.
    // `None` on boot/leaf/election bursts (no republish duty). Read-only (the cache is
    // filled by the ESP-NOW `SMOLv1 STAT` service arm, not here).
    stat_cache: Option<&CfgCache>,
    // #70/#49: this node's OWN compact DIAG record → retained `smol/<node_id>/diag`. Empty ⇒
    // skipped (boot/election bursts). Built by `RadioManager::diag_record` (uptime, boot-count,
    // reset reason, boot slot, otadata state, heap, flush/verify counters, button counts).
    diag: &[u8],
    // #70/#49: `Some` on a GATEWAY flush → after publishing THIS node's own diag, republish each
    // CACHED relayed-node DIAG record as retained `smol/<id>/diag` (leaves have no MQTT). F6
    // freshness-gated (an off-air node ages out, no ghost). `None` off-gateway. Read-only.
    diag_cache: Option<&RelayCache>,
    // #71: this node's OWN one-shot WiFi-scan record → retained `smol/<node_id>/scan`. Empty ⇒
    // skipped (the common case — a scan is only produced when a `W` command fired). Twin of `diag`.
    scan: &[u8],
    // #71: `Some` on a GATEWAY flush → republish each CACHED relayed-node SCAN record as retained
    // `smol/<id>/scan` (F6-gated). `None` off-gateway. Twin of `diag_cache`.
    scan_cache: Option<&RelayCache>,
    // #71: filled with the scan COMMANDS seen on the transient `smol/+/cmd/scan` topics this burst
    // (leaf targets to one-shot-relay `<id>W` + own flag). Twin of `reset_req`; NEVER cached.
    scan_req: &mut ScanReq,
    // #40 leaf-mesh-OTA: on a GATEWAY flush, filled with `(leaf_id, raw staged announce)`
    // when a retained `smol/<leaf>/ota/install = INSTALL` is present for a leaf id ≠ self
    // AND a staged image is available — the caller then relays it over ESP-NOW. The retained
    // cmd is CLEARED here (consumed) so an HA reload can't replay it. `&mut None` off-gateway.
    leaf_ota: &mut Option<(u8, crate::ota::Announce)>,
    // #40: PERSISTENT (caller-owned, survives across flushes) last raw staged announce. The
    // staged arm updates it when drained; the leaf-install pairing reads it — so a pair works
    // even if the staged retained wasn't re-drained in the SAME flush that consumed the
    // install (the race that left the install consumed but the relay never armed). `&mut None`
    // on boot/leaf bursts (no persistence needed).
    staged_raw: &mut Option<crate::ota::Announce>,
    // #40 headless diag + clear/retry: the last relay attempt's `(leaf_id, phase, clear)`.
    // PUBLISHED to `smol/<leaf>/ota/diag` here, and drives the retained-install clear (on a
    // terminal/exhausted phase) vs retry (transient). Consumed (set None) after publish.
    // `&mut None` off-gateway.
    leaf_diag: &mut Option<(u8, &'static str, bool, u8)>,
    // #3 RELAY RX-DIAG: the last relay's `(leaf_id, rx_any, otan_valid, last_wb, total)`.
    // PUBLISHED to retained `smol/<leaf>/ota/relaydiag` here, consumed after. `&mut None` off-gw.
    leaf_relay_rx: &mut Option<RelayDiag>,
    // #139-followup: on a failed SELF-fetch, `(chunk_k, chunk_n, retries, stalls, where)` → formatted +
    // published retained to `smol/<id>/ota/diag` (gateway-only; consumed once). `&mut None` otherwise.
    // #147: `where` = the `ota_fail::*` code for the exact stage that died (self-describing diag).
    ota_self_fail: &mut Option<(u32, u32, u32, u32, u32)>,
    deadline: Instant,
    tick: &mut dyn FnMut() -> bool,
) -> bool {
    // #100: broker = the ACTIVE slot's own-VLAN leg (runtime, from the NVS net-record).
    let (broker_ip, broker_port) = active_broker();
    let broker = (IpAddress::Ipv4(broker_ip), broker_port);
    // #21 per-board node-manager config topic (retained default-screen command).
    let mut cfg_topic = MqttScratch::new();
    let _ = write!(cfg_topic, "smol/{}/config/default_screen", node_id);
    // #33 Model-A per-board OTA install command topic (native HA Update button → INSTALL).
    let mut cmd_topic = MqttScratch::new();
    let _ = write!(cmd_topic, "smol/{}/ota/install", node_id);
    // #26 cast: this board's retained cast-enable topic (payload ON/OFF). feature=cast.
    #[cfg(feature = "cast")]
    let mut cast_topic = MqttScratch::new();
    #[cfg(feature = "cast")]
    let _ = write!(cast_topic, "smol/{}/cast", node_id);

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

    // F1 (oracle): 320→512. The full #12 discovery PUBLISH packet (topic ~38 B +
    // ~377 B JSON + MQTT framing ≈ 420 B) overflowed the old 320 B `pkt` →
    // `encode_publish` returned None → the publish was SILENTLY DROPPED (typed
    // entities never created). 512 holds it with margin (+ #27 peers + #21 CFG).
    let mut pkt = [0u8; 512];
    // F4: 512-B inbound accumulator in a `.bss` static (`MQTT_ACC`), NOT on the stack —
    // 256 overflowed on a long-url/signed OTA announce → the PUBLISH never accumulated →
    // announce silently never read. Static keeps the +256 off the mqtt_session frame.
    let acc: &mut [u8; 512] = unsafe { &mut *core::ptr::addr_of_mut!(MQTT_ACC) };
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
        recv_into(sockets, tcp_handle, &mut acc[..], &mut acc_len);
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

    // --- SUBSCRIBE order (subscribe before publishing so the broker queues the RETAINED
    // downlink payloads while we publish; all drained after the publishes, below) ---
    // #100 DRAIN-ORDER FIX (HW canary #110): the batt/grid/mc primary-downlink SUBSCRIBEs
    // (pids 1/2/3) are deferred to the END of the subscribe sequence — see the block just
    // before the PUBLISH loop. The retained-drain loop breaks on `got_batt && got_grid &&
    // got_mc && <400ms quiet>`; a broker delivers retained in SUBSCRIBE order, so subscribing
    // these three FIRST completed the break-gate before the later config retained (net/broker/
    // ota, pids 16-21) arrived in a subsequent TCP-window chunk >400ms later → the drain exited
    // and CFG-B/O never applied (deterministic miss, HW-observed). Subscribing them LAST makes
    // the gate structurally unable to trip until the whole config backlog is drained — order-
    // and buffer-size-independent. (The "subscribe before publish" priority is preserved: all
    // subscribes, batt/grid/mc included, still precede the telemetry PUBLISH.)
    // #33 Model-A: subscribe the ONE retained fleet STAGING topic (packet-id 4) as the
    // latest_version source + fetch target. No per-id announce-act topic exists (dropped
    // — the #32 closure): staging only advertises "update available"; it never fetches.
    if let Some(n) = crate::net::mqtt::encode_subscribe(&mut pkt, 4, OTA_STAGED_TOPIC) {
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
    // #26 cast: subscribe this board's retained cast-enable topic (pid 5). Reset the
    // flag to OFF FIRST so an absent / cleared retained topic reads as disabled — the
    // retained ON (if present) re-enables it during the drain below.
    #[cfg(feature = "cast")]
    {
        crate::net::cast::set_enabled(false);
        if let Some(n) = crate::net::mqtt::encode_subscribe(&mut pkt, 5, cast_topic.as_bytes()) {
            let _ = tcp_send(iface, device, sockets, tcp_handle, &pkt[..n], deadline, tick);
        }
    }
    // #21 leaf-relay: a GATEWAY (cfg_cache = Some) also subscribes the WILDCARD leaf
    // config topic (pid 8) so it caches every leaf's default-screen to relay over
    // ESP-NOW. `+` is a single-level MQTT wildcard → matches `smol/<any>/config/
    // default_screen`. The board's OWN config still arrives via `cfg_topic` (pid 6,
    // matched first below) → self-apply; the wildcard feeds ONLY other ids (§2).
    if cfg_cache.is_some() {
        if let Some(n) =
            crate::net::mqtt::encode_subscribe(&mut pkt, 8, b"smol/+/config/default_screen")
        {
            let _ = tcp_send(iface, device, sockets, tcp_handle, &pkt[..n], deadline, tick);
        }
        // #40 leaf-mesh-OTA (§B3): a GATEWAY also wildcard-subscribes the leaf OTA install
        // command (pid 9), twin of the config wildcard above → it acts on a leaf's native
        // HA Update Install button (`smol/<leaf>/ota/install = INSTALL`) by relaying the
        // staged image over ESP-NOW. The board's OWN install still arrives via `cmd_topic`
        // (pid 7) → self-OTA; the wildcard feeds ONLY other leaf ids.
        if let Some(n) = crate::net::mqtt::encode_subscribe(&mut pkt, 9, b"smol/+/ota/install") {
            let _ = tcp_send(iface, device, sockets, tcp_handle, &pkt[..n], deadline, tick);
        }
        // #48 blue-LED mode (pid 10): wildcard-subscribe every leaf's retained led config so the
        // gateway caches + relays it (key `L`) over ESP-NOW, twin of the default_screen wildcard.
        if let Some(n) = crate::net::mqtt::encode_subscribe(&mut pkt, 10, b"smol/+/config/led") {
            let _ = tcp_send(iface, device, sockets, tcp_handle, &pkt[..n], deadline, tick);
        }
        // #43 display units (pid 11): the GLOBAL retained units topic `smol/config/units` (NO id
        // — one setting for the whole fleet, so NOT a `smol/+/…` wildcard). The gateway caches it
        // under the broadcast target CFG_TARGET_ALL (255) so ONE relayed `<255>U<val>` frame
        // reaches every leaf, and self-applies its own display units (gw_own.units) below.
        if let Some(n) = crate::net::mqtt::encode_subscribe(&mut pkt, 11, b"smol/config/units") {
            let _ = tcp_send(iface, device, sockets, tcp_handle, &pkt[..n], deadline, tick);
        }
        // #55 plugin visibility (pid 12): wildcard-subscribe every leaf's retained plugin mask so
        // the gateway caches + relays it (key `P`) over ESP-NOW, twin of the led wildcard.
        if let Some(n) = crate::net::mqtt::encode_subscribe(&mut pkt, 12, b"smol/+/config/plugins") {
            let _ = tcp_send(iface, device, sockets, tcp_handle, &pkt[..n], deadline, tick);
        }
        // #52 remote reboot (pid 13): wildcard-subscribe the TRANSIENT `smol/+/cmd/reset` COMMAND
        // topic (retain:false → seen only while we're connected, never replayed). On receipt the
        // gateway fires a ONE-SHOT `<id>R` relay (never cached — anti-reboot-loop); own id → self.
        if let Some(n) = crate::net::mqtt::encode_subscribe_qos1(&mut pkt, 13, b"smol/+/cmd/reset") {
            let _ = tcp_send(iface, device, sockets, tcp_handle, &pkt[..n], deadline, tick);
        }
        // #71 on-demand scan (pid 14): wildcard-subscribe the TRANSIENT `smol/+/cmd/scan` COMMAND
        // topic (retain:false). On receipt the gateway fires a ONE-SHOT `<id>W` relay (never cached
        // — a periodic scan is the coexist hazard); own id → self-scan. Twin of the pid-13 reset arm.
        if let Some(n) = crate::net::mqtt::encode_subscribe_qos1(&mut pkt, 14, b"smol/+/cmd/scan") {
            let _ = tcp_send(iface, device, sockets, tcp_handle, &pkt[..n], deadline, tick);
        }
        // #45 Custom screen (pid 15): wildcard-subscribe every leaf's retained custom-screen layout
        // so the gateway caches + relays it (key `Y`) over ESP-NOW, twin of the led/plugins wildcard.
        // (pid 15 — #71's scan took 14 in the merge; distinct pid per concurrent SUBSCRIBE.)
        if let Some(n) = crate::net::mqtt::encode_subscribe(&mut pkt, 15, b"smol/+/config/custom") {
            let _ = tcp_send(iface, device, sockets, tcp_handle, &pkt[..n], deadline, tick);
        }
        // #100 network-switch (pid 16 per-node + pid 17 fleet-wide): the retained active-slot index.
        // CONFIG/CACHED (key `N`, relayed like S/L/U/P/Y). Per-node `smol/<id>/config/net` + the
        // global `smol/config/net` (→ target 255). Applying it writes the NVS net-record + reboots
        // into the slot, edge-triggered on a commanded-slot CHANGE (a re-read is a no-op → no loop).
        if let Some(n) = crate::net::mqtt::encode_subscribe(&mut pkt, 16, b"smol/+/config/net") {
            let _ = tcp_send(iface, device, sockets, tcp_handle, &pkt[..n], deadline, tick);
        }
        if let Some(n) = crate::net::mqtt::encode_subscribe(&mut pkt, 17, b"smol/config/net") {
            let _ = tcp_send(iface, device, sockets, tcp_handle, &pkt[..n], deadline, tick);
        }
        // #100 Stage 2 broker override (pid 18 per-node + pid 19 fleet-wide): the retained broker leg.
        // Twin of net (key `B`, relayed + cached + edge-triggered reboot). Applying it writes the NVS
        // net-record + reboots onto the new broker; a wrong value self-heals via the CONNACK fallback.
        if let Some(n) = crate::net::mqtt::encode_subscribe(&mut pkt, 18, b"smol/+/config/broker") {
            let _ = tcp_send(iface, device, sockets, tcp_handle, &pkt[..n], deadline, tick);
        }
        if let Some(n) = crate::net::mqtt::encode_subscribe(&mut pkt, 19, b"smol/config/broker") {
            let _ = tcp_send(iface, device, sockets, tcp_handle, &pkt[..n], deadline, tick);
        }
        // #100 Stage 3 OTA-host override (pid 20 per-node + pid 21 fleet-wide): one extra RFC1918 image
        // host (key `O`, relayed + cached). Applied WITHOUT reboot — the allowlist is read at fetch time.
        if let Some(n) = crate::net::mqtt::encode_subscribe(&mut pkt, 20, b"smol/+/config/ota_host") {
            let _ = tcp_send(iface, device, sockets, tcp_handle, &pkt[..n], deadline, tick);
        }
        if let Some(n) = crate::net::mqtt::encode_subscribe(&mut pkt, 21, b"smol/config/ota_host") {
            let _ = tcp_send(iface, device, sockets, tcp_handle, &pkt[..n], deadline, tick);
        }
        // #72 IO registry (pid 22): wildcard-subscribe every node's retained pin-map so the
        // gateway caches + relays it (key `G`) over ESP-NOW, twin of the custom/led wildcard.
        // `io`-gated (⊃ espnow) → non-io builds emit no SUBSCRIBE. (pids 18-21 are #100/#110's
        // broker/ota_host per-node+global — #72 is last on the merge ladder, so it takes 22/23.)
        // Kept BEFORE the batt/grid/mc drain-gate below (that ordering is load-bearing — see #110).
        #[cfg(feature = "io")]
        if let Some(n) = crate::net::mqtt::encode_subscribe(&mut pkt, 22, b"smol/+/config/io") {
            let _ = tcp_send(iface, device, sockets, tcp_handle, &pkt[..n], deadline, tick);
        }
        // #72 IO output control (pid 23): every node's retained output-states topic (key `g`).
        #[cfg(feature = "io")]
        if let Some(n) = crate::net::mqtt::encode_subscribe(&mut pkt, 23, b"smol/+/io/set") {
            let _ = tcp_send(iface, device, sockets, tcp_handle, &pkt[..n], deadline, tick);
        }
    }

    // #100 DRAIN-ORDER FIX (HW canary #110): the batt/grid/mc primary downlinks are the
    // retained-drain break-gate (`got_batt && got_grid && got_mc && <quiet>`), so they MUST be
    // the LAST retained to arrive — subscribe them AFTER every config topic above. A broker sends
    // retained in SUBSCRIBE order, so the gate now cannot complete until the whole config backlog
    // (screen/led/…/net/broker/ota/io, pids 6-23) has been delivered and drained → CFG reliably
    // applies. (Boot burst: cfg_cache=None skips the gateway block, so these are effectively first,
    // as before — mc is usually absent at boot, so that path still drains to the deadline.) Pids
    // stay 1/2/3 (identifiers, not order); only the SEND order moved. Still before the PUBLISH loop.
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
    // #155 channel-drag operator lever: subscribe the retained channel-hint (packet-id 13 — the
    // first free id; 9 is `smol/+/ota/install`). Sent right after the election topic so its retained
    // value rides the SAME broker burst as `MC` and is captured before the resolver runs (the settle
    // window after the primary downlinks catches it, exactly like the OTA/config retained topics).
    if let Some(n) = crate::net::mqtt::encode_subscribe(&mut pkt, 13, MESH_CHANNEL_HINT_TOPIC) {
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
        // #12: THREE TYPED discovery configs (retained) instead of the old single
        // `telemetry/config`. The old single config (name "smol <id>" + no
        // has_entity_name) made HA concatenate the device name → the doubled
        // `sensor.smol_<id>_dominion_smol_<id>`. Each typed config carries
        // has_entity_name + a terse name + an explicit object_id (kills the doubling)
        // + the SAME device block (grouped under the one device) + a value_template
        // that extracts one field from the UNCHANGED space-positional telemetry line
        // (`<tempInt>F <volt.1f>V <status…>`) — CONFIG-ONLY, no payload change.
        // Templates use single quotes internally → no `\"`-escaping; they're passed as
        // `&str` args so their Jinja `{ }` need no `{{`/`}}` escaping. All length-guarded
        // → a short/garbage line yields "" (never an HA template error).
        let noun = crate::net::names::name_for_id(id).1;
        let cfgs: [(&str, &str, &str, &str); 3] = [
            (
                "temp", "Temp",
                "{% set p = value.split(' ') %}{{ p[0][:-1] if p|length>0 and p[0].endswith('F') else '' }}",
                ",\"unit_of_measurement\":\"°F\",\"device_class\":\"temperature\"",
            ),
            (
                "voltage", "Voltage",
                "{% set p = value.split(' ') %}{{ p[1][:-1] if p|length>1 and p[1].endswith('V') else '' }}",
                ",\"unit_of_measurement\":\"V\",\"device_class\":\"voltage\"",
            ),
            (
                "status", "Status",
                "{% set p = value.split(' ') %}{{ p[2:]|join(' ') if p|length>2 else '' }}",
                "",
            ),
        ];
        for (field, name, tmpl, extra) in cfgs {
            if Instant::now() > deadline {
                break;
            }
            let mut dtopic = MqttScratch::new();
            let _ = write!(dtopic, "homeassistant/sensor/smol{}/{}/config", id, field);
            // F1: build the 373-B config in the `.bss` static JsonScratch (512), NOT a
            // stack MqttScratch — keeps the oversized payload off the mqtt_session frame.
            // Single-caller → the &'static mut borrow is alias-safe; clear() per config.
            let json = unsafe { &mut *core::ptr::addr_of_mut!(MQTT_JSON) };
            json.clear();
            let _ = write!(
                json,
                "{{\"unique_id\":\"smol{}_{}\",\"object_id\":\"smol_{}_{}\",\"has_entity_name\":true,\"name\":\"{}\",\"state_topic\":\"smol/{}/telemetry\",\"value_template\":\"{}\"{},\"expire_after\":300,\"device\":{{\"identifiers\":[\"smol{}\"],\"name\":\"smol {} {}\"}}}}",
                id, field, id, field, name, id, tmpl, extra, id, id, noun
            );
            if let Some(n) = crate::net::mqtt::encode_publish(&mut pkt, dtopic.as_bytes(), json.as_bytes(), true) {
                let _ = tcp_send(iface, device, sockets, tcp_handle, &pkt[..n], deadline, tick);
            }
        }
    }

    // #64: publish THIS gateway's WiFi-uplink RSSI (RSSI-to-AP). Only the associated
    // gateway has it — leaves are ESP-NOW-only — so it's published for `node_id` (self)
    // alone, not per queued leaf. `elect.my_rssi` carries the caller's last-good capture;
    // -99 = "never associated" sentinel → skip (a real gateway link is never that weak).
    // Transient value topic + a RETAINED discovery config auto-creates
    // `sensor.smol_<id>_uplink`; `expire_after` clears a demoted node's stale reading, so
    // the value follows the crown across #51 role swaps with no dynamic-owner template.
    if elect.my_rssi > -99 {
        let mut utopic = MqttScratch::new();
        let _ = write!(utopic, "smol/{}/uplink", node_id);
        let mut uval = MqttScratch::new();
        let _ = write!(uval, "{}", elect.my_rssi);
        if let Some(n) =
            crate::net::mqtt::encode_publish(&mut pkt, utopic.as_bytes(), uval.as_bytes(), false)
        {
            let _ = tcp_send(iface, device, sockets, tcp_handle, &pkt[..n], deadline, tick);
        }
        let mut dtopic = MqttScratch::new();
        let _ = write!(dtopic, "homeassistant/sensor/smol{}/uplink/config", node_id);
        let unoun = crate::net::names::name_for_id(node_id).1;
        // F1 discipline: build the config in the .bss JsonScratch (512), not on-stack.
        let json = unsafe { &mut *core::ptr::addr_of_mut!(MQTT_JSON) };
        json.clear();
        let _ = write!(
            json,
            // Mirror the telemetry discovery pattern EXACTLY (terse name + object_id, NO
            // entity_category) so HA derives the clean entity_id `sensor.smol_<id>_uplink`
            // from object_id instead of the device-name-concatenated form. unique_id bumped
            // (_uplk) to force HA to re-derive the entity_id for the corrected config.
            "{{\"unique_id\":\"smol{}_uplk\",\"object_id\":\"smol_{}_uplink\",\"has_entity_name\":true,\"name\":\"Uplink\",\"state_topic\":\"smol/{}/uplink\",\"unit_of_measurement\":\"dBm\",\"device_class\":\"signal_strength\",\"expire_after\":120,\"device\":{{\"identifiers\":[\"smol{}\"],\"name\":\"smol {} {}\"}}}}",
            node_id, node_id, node_id, node_id, node_id, unoun
        );
        if let Some(n) =
            crate::net::mqtt::encode_publish(&mut pkt, dtopic.as_bytes(), json.as_bytes(), true)
        {
            let _ = tcp_send(iface, device, sockets, tcp_handle, &pkt[..n], deadline, tick);
        }
    }

    // #27: publish this node's serialized roster as RETAINED `smol/<id>/peers`, if any.
    // GATEWAY-primary — the caller (`flush_telemetry`) serializes the roster and passes
    // it; leaves / the boot + election-only bursts pass an empty slice (skipped here).
    // One retained publish per flush ≈ the ~30 s topology heartbeat; identical
    // encode_publish/tcp_send path as the discovery configs above.
    if !peers.is_empty() {
        let mut ptopic = MqttScratch::new();
        let _ = write!(ptopic, "smol/{}/peers", node_id);
        if let Some(n) = crate::net::mqtt::encode_publish(&mut pkt, ptopic.as_bytes(), peers, true) {
            let _ = tcp_send(iface, device, sockets, tcp_handle, &pkt[..n], deadline, tick);
        }
    }

    // #50: publish this node's LIVE screen:page as RETAINED `smol/<id>/status`, if any.
    // Same path as peers — the caller passes `STAT|<screen>:<page>` from the live render
    // state (`App::live_screen`, captures manual BOOT-nav); empty ⇒ skipped (boot/election
    // bursts). Backward-compat: purely additive, a new topic (old HA/fw ignore it).
    if !status.is_empty() {
        let mut stopic = MqttScratch::new();
        let _ = write!(stopic, "smol/{}/status", node_id);
        if let Some(n) = crate::net::mqtt::encode_publish(&mut pkt, stopic.as_bytes(), status, true) {
            let _ = tcp_send(iface, device, sockets, tcp_handle, &pkt[..n], deadline, tick);
        }
    }

    // #50b: republish each CACHED leaf status (filled by the ESP-NOW `SMOLv1 STAT` service
    // arm — leaves have no MQTT) as RETAINED `smol/<leaf>/status`. Skip our OWN id (self-
    // published just above via #50a) and empty values. Prepend `STAT|` so EVERY status
    // topic is uniform (`STAT|<screen>:<page>`), self + leaves, for the one HA template.
    if let Some(sc) = stat_cache {
        // #68 F6: freshness-gate the status republish. An off-air leaf's entry goes stale →
        // its retained smol/<id>/status STOPS refreshing → HA sees it age out instead of a
        // perpetually-fresh ghost (the ghost that faked id8-alive + masked id9's floor-wipe).
        let now_ms = Instant::now().duration_since_epoch().as_millis();
        for i in 0..sc.count() {
            // #68 F6 freshness gate; #56: the stat cache is single-channel, key column inert here.
            if let Some((lid, val)) = sc.entry_fresh(i, now_ms, STAT_FRESH_MS) {
                if lid == node_id || val.is_empty() {
                    continue;
                }
                let mut ltopic = MqttScratch::new();
                let _ = write!(ltopic, "smol/{}/status", lid);
                let mut sbuf = [0u8; 5 + CFG_VALUE_MAX];
                sbuf[..5].copy_from_slice(b"STAT|");
                let m = val.len().min(CFG_VALUE_MAX);
                sbuf[5..5 + m].copy_from_slice(&val[..m]);
                if let Some(n) =
                    crate::net::mqtt::encode_publish(&mut pkt, ltopic.as_bytes(), &sbuf[..5 + m], true)
                {
                    let _ = tcp_send(iface, device, sockets, tcp_handle, &pkt[..n], deadline, tick);
                }
            }
        }
    }

    // #70/#49: publish this node's OWN compact DIAG record as RETAINED `smol/<id>/diag`, if any.
    // ONE record — HA parses it package-side (no per-signal discovery, per the #70 contract).
    if !diag.is_empty() {
        let mut dtopic = MqttScratch::new();
        let _ = write!(dtopic, "smol/{}/diag", node_id);
        if let Some(n) = crate::net::mqtt::encode_publish(&mut pkt, dtopic.as_bytes(), diag, true) {
            let _ = tcp_send(iface, device, sockets, tcp_handle, &pkt[..n], deadline, tick);
        }
    }

    // #70/#49: republish each CACHED relayed-node DIAG record (filled by the ESP-NOW `SMOLv1 DIAG`
    // service arm — leaves have no MQTT) as RETAINED `smol/<id>/diag`. Skip our OWN id (published
    // just above) + empty values. Verbatim (the node already formatted the key=val record). #68 F6
    // freshness-gated: an off-air node's entry ages out → its retained diag stops refreshing.
    if let Some(dc) = diag_cache {
        let now_ms = Instant::now().duration_since_epoch().as_millis();
        for i in 0..dc.count() {
            // #70 F6: DIAG's own (longer) freshness window — the ~60 s diag cadence would flicker
            // stale under STAT's 45 s gate. A node that missed ~2 diags is genuinely gone.
            if let Some((nid, val)) = dc.entry_fresh(i, now_ms, DIAG_FRESH_MS) {
                if nid == node_id || val.is_empty() {
                    continue;
                }
                let mut dtopic = MqttScratch::new();
                let _ = write!(dtopic, "smol/{}/diag", nid);
                if let Some(n) = crate::net::mqtt::encode_publish(&mut pkt, dtopic.as_bytes(), val, true)
                {
                    let _ = tcp_send(iface, device, sockets, tcp_handle, &pkt[..n], deadline, tick);
                }
            }
        }
    }

    // #71: publish this node's OWN one-shot WiFi-scan record as RETAINED `smol/<id>/scan`, if a
    // scan was produced this cycle (empty = no scan → skipped). Twin of the diag own-publish.
    if !scan.is_empty() {
        let mut stopic = MqttScratch::new();
        let _ = write!(stopic, "smol/{}/scan", node_id);
        if let Some(n) = crate::net::mqtt::encode_publish(&mut pkt, stopic.as_bytes(), scan, true) {
            let _ = tcp_send(iface, device, sockets, tcp_handle, &pkt[..n], deadline, tick);
        }
    }

    // #71: republish each CACHED relayed-node SCAN record (filled by the `SMOLv1 SCAN` service
    // arm — leaves have no MQTT) as RETAINED `smol/<id>/scan`. Skip OWN id + empty. Twin of the
    // diag republish, F6-gated on DIAG_FRESH_MS (a scan is one-shot + on-demand, so an old cached
    // scan aging out is correct — it reflects the last-heard environment, not a live stream).
    if let Some(sc) = scan_cache {
        let now_ms = Instant::now().duration_since_epoch().as_millis();
        for i in 0..sc.count() {
            if let Some((nid, val)) = sc.entry_fresh(i, now_ms, DIAG_FRESH_MS) {
                if nid == node_id || val.is_empty() {
                    continue;
                }
                let mut stopic = MqttScratch::new();
                let _ = write!(stopic, "smol/{}/scan", nid);
                if let Some(n) = crate::net::mqtt::encode_publish(&mut pkt, stopic.as_bytes(), val, true)
                {
                    let _ = tcp_send(iface, device, sockets, tcp_handle, &pkt[..n], deadline, tick);
                }
            }
        }
    }

    // #74 stage-2 display mirror: publish the gateway's OWN glass as a 64×32 1-bit BMP (base64) to
    // RETAINED `smol/<id>/screen` for HA to render as an `mqtt image`. `cast`-only — it reuses the
    // #26 Cast tee's live `Mirror` (the tee already snapshots the glass every render tick, so there
    // is NO new draw-path tap; the invasive per-plugin text approach was deliberately not taken).
    // GATEWAY-only v1: leaves have no MQTT (Cast is a gateway-role activity) — a leaf-screen mirror
    // is a mesh-frame follow-on. The b64 BMP is ~424 B → fits the 512 B `pkt` packet with margin.
    #[cfg(feature = "cast")]
    {
        let mut sctopic = MqttScratch::new();
        let _ = write!(sctopic, "smol/{}/screen", node_id);
        crate::net::cast::with_screen_b64(|b64| {
            if let Some(n) =
                crate::net::mqtt::encode_publish(&mut pkt, sctopic.as_bytes(), b64, true)
            {
                let _ = tcp_send(iface, device, sockets, tcp_handle, &pkt[..n], deadline, tick);
            }
        });
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
    // #40 QUIET-PERIOD (settle-break truncation fix): the drain must not exit until the retained
    // backlog is COMPLETE. The batt/grid/mc "downlink complete" flags key on three RETAINED
    // topics the broker delivers instantly on SUBACK, so a FIXED window from "3 flags complete"
    // broke BEFORE the rest of the retained burst (staged/install/cfg) arrived → truncated them
    // (armdiag staged_raw/install-caught/cfg_cache all none; the arm only fired on the rare burst
    // ordering that front-loaded the OTA topics → the ~30-min "slow arm"). Fix: break only after
    // the 3 flags AND a QUIET GAP — no new packet for DOWNLINK_SETTLE — so the drain rides out the
    // whole retained burst regardless of order. `last_msg` resets on every parsed packet below.
    const DOWNLINK_SETTLE: Duration = Duration::from_millis(400);
    let mut last_msg: Instant = Instant::now();
    // #40: the RAW (ungated) staged announce + a leaf id whose retained install cmd is
    // present. Paired AFTER the drain (both are retained → arrive in the same broker burst)
    // → `*leaf_ota`. The staged is the caller-PERSISTED `staged_raw` (updated below when a
    // staged retained is drained), so an install is paired with the last-known staged even
    // if THIS flush didn't re-drain it. Raw (not the gate()d `ota_offer`) because the LEAF,
    // not the gateway, owns the freshness gate (the OTAM handler rejects `build ≤ leaf.build`);
    // the gateway may be NEWER than the leaf's target, so a gateway-build gate would drop it.
    let mut pending_leaf: Option<u8> = None;
    // #111: every leaf id whose retained `smol/<id>/ota/install=INSTALL` was SEEN this burst
    // (deduped, bounded). Unlike `pending_leaf` (last-wins, one relay/burst) this is the FULL
    // armed set — the version-flip cleanup below clears a completed leaf's order even when it is
    // not the one being relayed this burst (e.g. it installed under a PRIOR crown tenure). Dropped
    // extras are re-seen next burst (retained), so an 8-slot cap never loses an order.
    let mut armed_installs = [0u8; RESET_REQ_MAX];
    let mut armed_n = 0usize;
    loop {
        if tick() {
            break; // #20 abort during downlink wait → fall through to clean DISCONNECT
        }
        iface.poll(smoltcp_now(), device, sockets);
        recv_into(sockets, tcp_handle, &mut acc[..], &mut acc_len);
        loop {
            let (consumed, puback_id) = match crate::net::mqtt::parse_packet(&acc[..acc_len]) {
                None => break,
                Some((crate::net::mqtt::Incoming::Publish { topic, payload, packet_id }, consumed)) => {
                    // #26 cast: precompute the cast-topic match as a plain bool (avoids a
                    // block-in-`if`-condition; `false` in non-cast builds, where `cast_topic`
                    // does not exist, so the arm below is inert there).
                    #[cfg(feature = "cast")]
                    let is_cast_topic = topic == cast_topic.as_bytes();
                    #[cfg(not(feature = "cast"))]
                    let is_cast_topic = false;
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
                    } else if topic == MESH_CHANNEL_HINT_TOPIC {
                        // #155: capture the operator's retained channel hint into the election
                        // record; the claim gate (below, next to the #146 guard) honors it. An
                        // empty/garbage payload parses to None → no hint → unchanged election.
                        elect.channel_hint = parse_channel_hint(payload);
                        if let Some(h) = elect.channel_hint {
                            log::info!("smol: #155 channel_hint = ch{} (retained)", h);
                        }
                    } else if topic == OTA_STAGED_TOPIC {
                        // #33 Model-A: parse + GATE the retained STAGED line (monotonicity +
                        // host allowlist + size). A gate-passing staged build becomes the
                        // latest_version + fetch TARGET (ota_offer) — but it does NOT fetch;
                        // the fetch is AND-gated on this board's own `install` command below.
                        // Stale/foreign/oversize → ignored (up-to-date; no offer).
                        match crate::ota::parse_announce(payload) {
                            Some(a) => {
                                // #40: PERSIST the RAW announce (pre-gate) for a possible leaf
                                // relay — the leaf owns its own freshness gate, so a
                                // gateway-build gate must NOT drop it here.
                                *staged_raw = Some(a);
                                match crate::ota::gate(&a) {
                                    Ok(()) => {
                                        log::info!(
                                            "smol OTA: staged build {} available (running {})",
                                            a.build,
                                            crate::ota::BUILD_NUMBER
                                        );
                                        *ota_offer = Some(a);
                                    }
                                    Err(why) => log::info!(
                                        "smol OTA: staged build {} not newer/ineligible ({:?})",
                                        a.build,
                                        why
                                    ),
                                }
                            }
                            None => log::warn!("smol OTA: malformed staged line ignored"),
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
                        // #33 Model-A: the native HA Update INSTALL command. PANIC-FREE
                        // exact byte compare only (untrusted RETAINED payload = boot-loop-
                        // brick class): `INSTALL` → arm the flag (the caller AND-gates the
                        // fetch on the already-`gate()`d staged target — this bool never
                        // itself touches flash); any other bytes → ignore. Cleared (empty
                        // retained publish) below so it can't replay next boot.
                        if payload == b"INSTALL" {
                            *install_requested = true;
                            log::info!("smol #33: OTA install command received");
                        }
                    } else if is_cast_topic {
                        // Untrusted RETAINED payload → exact byte compare only (same
                        // discipline as the OTA install arm): ON/on/1 enables the WLED
                        // pixel-stream for this gateway's flush; anything else disables.
                        #[cfg(feature = "cast")]
                        {
                            let on = payload == b"ON" || payload == b"on" || payload == b"1";
                            crate::net::cast::set_enabled(on);
                            log::info!("smol #26: cast {}", if on { "ENABLED" } else { "disabled" });
                        }
                    } else if let Some(leaf_id) = parse_leaf_config_topic(topic, b"/config/default_screen") {
                        // #21 leaf-relay: a wildcard-delivered OTHER leaf's config. Cache
                        // the verbatim value bytes for the ESP-NOW relay (mode.rs
                        // broadcast_cached_configs). `leaf_id == node_id` is the gateway's
                        // OWN config — already handled by the `cfg_topic` arm above; guard
                        // anyway. Only present when cfg_cache = Some (gateway flush).
                        if leaf_id != node_id {
                            if let Some(cache) = cfg_cache.as_deref_mut() {
                                // #56: `default_screen` is the SCREEN channel → cache under key
                                // `S` (#48 led / #43 units / #55 plugins add their own topic + fill
                                // site; the relay machinery is already key-generic).
                                // #68: cfg_cache is DOWNLINK (never mac-queried) → zero MAC; the
                                // timestamp is set for API uniformity (cfg_cache isn't freshness-gated).
                                let now = Instant::now().duration_since_epoch().as_millis();
                                cache.set(leaf_id, CFG_KEY_SCREEN, payload, [0u8; 6], now);
                                log::info!(
                                    "smol #21/#56: cached leaf id{} screen config for relay ({} B)",
                                    leaf_id,
                                    payload.len()
                                );
                            }
                        }
                    } else if let Some(leaf_id) = parse_leaf_config_topic(topic, b"/config/led") {
                        // #48 blue-LED mode: twin of the default_screen arm. OTHER leaf → cache
                        // under key `L` for the ESP-NOW relay; OUR OWN id → capture into gw_own so
                        // `service()` self-applies it (the gateway reads its own led directly, not
                        // relayed). Verbatim bytes; the leaf's LedMode::from_wire validates.
                        if leaf_id != node_id {
                            if let Some(cache) = cfg_cache.as_deref_mut() {
                                let now = Instant::now().duration_since_epoch().as_millis();
                                cache.set(leaf_id, CFG_KEY_LED, payload, [0u8; 6], now);
                                log::info!(
                                    "smol #48: cached leaf id{} led config for relay ({} B)",
                                    leaf_id,
                                    payload.len()
                                );
                            }
                        } else {
                            gw_own.led = Some(GwOwnCfg::val(payload));
                            log::info!("smol #48: gateway own led config captured ({} B)", payload.len());
                        }
                    } else if topic == b"smol/config/units" {
                        // #43 display units — GLOBAL (no id in the topic → an exact match, not the
                        // `smol/<id>/…` wildcard parse). TWO effects, mirroring the own-led branch
                        // above but fleet-wide: (1) cache under the broadcast target CFG_TARGET_ALL
                        // so ONE relayed `<255>U<val>` frame reaches every leaf; (2) capture into
                        // gw_own so `service()` self-applies the gateway's OWN display units. The
                        // bytes are opaque here — the leaf's `Units::from_wire` validates them
                        // (garbage/partial → keep current, #46 clamp).
                        if let Some(cache) = cfg_cache.as_deref_mut() {
                            let now = Instant::now().duration_since_epoch().as_millis();
                            cache.set(CFG_TARGET_ALL, CFG_KEY_UNITS, payload, [0u8; 6], now);
                        }
                        gw_own.units = Some(GwOwnCfg::val(payload));
                        log::info!(
                            "smol #43: global display units captured ({} B) — cached (255,U) + self",
                            payload.len()
                        );
                    } else if let Some(leaf_id) = parse_leaf_config_topic(topic, b"/config/plugins") {
                        // #55 plugin visibility: twin of the led arm. OTHER leaf → cache under key
                        // `P` for the ESP-NOW relay; OUR OWN id → capture into gw_own so `service()`
                        // self-applies it. Verbatim bytes; the leaf's `parse_plugin_mask` validates
                        // (bad/partial hex → keep current mask, never a blank menu).
                        if leaf_id != node_id {
                            if let Some(cache) = cfg_cache.as_deref_mut() {
                                let now = Instant::now().duration_since_epoch().as_millis();
                                cache.set(leaf_id, CFG_KEY_PLUGINS, payload, [0u8; 6], now);
                                log::info!(
                                    "smol #55: cached leaf id{} plugin mask for relay ({} B)",
                                    leaf_id,
                                    payload.len()
                                );
                            }
                        } else {
                            gw_own.plugins = Some(GwOwnCfg::val(payload));
                            log::info!("smol #55: gateway own plugin mask captured ({} B)", payload.len());
                        }
                    } else if let Some(leaf_id) = parse_leaf_config_topic(topic, b"/config/custom") {
                        // #45 Custom screen: twin of the led/plugins arm. OTHER leaf → cache under
                        // key `Y` for the ESP-NOW relay; OUR OWN id → capture into gw_own so
                        // `service()` self-applies it. Verbatim RESOLVED bytes (entities already
                        // substituted HA-side); the Custom plugin parses the layout wire panic-free.
                        if leaf_id != node_id {
                            if let Some(cache) = cfg_cache.as_deref_mut() {
                                let now = Instant::now().duration_since_epoch().as_millis();
                                cache.set(leaf_id, CFG_KEY_CUSTOM, payload, [0u8; 6], now);
                                log::info!(
                                    "smol #45: cached leaf id{} custom screen for relay ({} B)",
                                    leaf_id,
                                    payload.len()
                                );
                            }
                        } else {
                            gw_own.custom = Some(GwOwnCfg::val(payload));
                            log::info!("smol #45: gateway own custom screen captured ({} B)", payload.len());
                        }
                    } else if topic == b"smol/config/net" {
                        // #100 GLOBAL network-switch (no id) → cache under the broadcast target 255
                        // so ONE relayed `<255>N<slot>` frame reaches every leaf, + gw_own.net for the
                        // gateway's own self-apply. Value = one ASCII slot digit; the apply validates.
                        if let Some(cache) = cfg_cache.as_deref_mut() {
                            let now = Instant::now().duration_since_epoch().as_millis();
                            cache.set(CFG_TARGET_ALL, CFG_KEY_NET, payload, [0u8; 6], now);
                        }
                        gw_own.net = Some(GwOwnCfg::val(payload));
                        log::info!(
                            "smol #100: global net-slot captured ({} B) — cached (255,N) + self",
                            payload.len()
                        );
                    } else if let Some(leaf_id) = parse_leaf_config_topic(topic, b"/config/net") {
                        // #100 PER-NODE network-switch: twin of the led/plugins arm. OTHER leaf →
                        // cache under key `N` for the ESP-NOW relay; OUR OWN id → gw_own.net. The apply
                        // (main) writes the NVS net-record + reboots into the slot, edge-triggered on a
                        // commanded-slot CHANGE (a re-read of the same retained value is a no-op → no loop).
                        if leaf_id != node_id {
                            if let Some(cache) = cfg_cache.as_deref_mut() {
                                let now = Instant::now().duration_since_epoch().as_millis();
                                cache.set(leaf_id, CFG_KEY_NET, payload, [0u8; 6], now);
                                log::info!(
                                    "smol #100: cached leaf id{} net-slot for relay ({} B)",
                                    leaf_id,
                                    payload.len()
                                );
                            }
                        } else {
                            gw_own.net = Some(GwOwnCfg::val(payload));
                            log::info!("smol #100: gateway own net-slot captured ({} B)", payload.len());
                        }
                    } else if topic == b"smol/config/broker" {
                        // #100 Stage 2 GLOBAL broker override (no id) → cache under target 255 + gw_own.
                        // Twin of the global-net arm. Opaque bytes; the apply (main) RFC1918-validates.
                        if let Some(cache) = cfg_cache.as_deref_mut() {
                            let now = Instant::now().duration_since_epoch().as_millis();
                            cache.set(CFG_TARGET_ALL, CFG_KEY_BROKER, payload, [0u8; 6], now);
                        }
                        gw_own.broker = Some(GwOwnCfg::val(payload));
                        log::info!(
                            "smol #100: global broker override captured ({} B) — cached (255,B) + self",
                            payload.len()
                        );
                    } else if let Some(leaf_id) = parse_leaf_config_topic(topic, b"/config/broker") {
                        // #100 Stage 2 PER-NODE broker override: twin of the per-node net arm. OTHER
                        // leaf → cache under key `B` for the ESP-NOW relay; OUR OWN id → gw_own.broker.
                        if leaf_id != node_id {
                            if let Some(cache) = cfg_cache.as_deref_mut() {
                                let now = Instant::now().duration_since_epoch().as_millis();
                                cache.set(leaf_id, CFG_KEY_BROKER, payload, [0u8; 6], now);
                                log::info!(
                                    "smol #100: cached leaf id{} broker override for relay ({} B)",
                                    leaf_id,
                                    payload.len()
                                );
                            }
                        } else {
                            gw_own.broker = Some(GwOwnCfg::val(payload));
                            log::info!("smol #100: gateway own broker override captured ({} B)", payload.len());
                        }
                    } else if topic == b"smol/config/ota_host" {
                        // #100 Stage 3 GLOBAL OTA-host override (no id) → cache under target 255 + gw_own.
                        // Twin of the global-net arm. Opaque bytes; the apply (main) RFC1918-validates.
                        if let Some(cache) = cfg_cache.as_deref_mut() {
                            let now = Instant::now().duration_since_epoch().as_millis();
                            cache.set(CFG_TARGET_ALL, CFG_KEY_OTA, payload, [0u8; 6], now);
                        }
                        gw_own.ota = Some(GwOwnCfg::val(payload));
                        log::info!(
                            "smol #100: global OTA-host override captured ({} B) — cached (255,O) + self",
                            payload.len()
                        );
                    } else if let Some(leaf_id) = parse_leaf_config_topic(topic, b"/config/ota_host") {
                        // #100 Stage 3 PER-NODE OTA-host override: twin of the per-node net arm. OTHER
                        // leaf → cache under key `O` for the ESP-NOW relay; OUR OWN id → gw_own.ota.
                        if leaf_id != node_id {
                            if let Some(cache) = cfg_cache.as_deref_mut() {
                                let now = Instant::now().duration_since_epoch().as_millis();
                                cache.set(leaf_id, CFG_KEY_OTA, payload, [0u8; 6], now);
                                log::info!(
                                    "smol #100: cached leaf id{} OTA-host override for relay ({} B)",
                                    leaf_id,
                                    payload.len()
                                );
                            }
                        } else {
                            gw_own.ota = Some(GwOwnCfg::val(payload));
                            log::info!("smol #100: gateway own OTA-host override captured ({} B)", payload.len());
                        }
                    } else if let Some(_leaf_id) = parse_leaf_config_topic(topic, b"/config/io") {
                        // #72 IO registry pin-map: twin of the custom/net arms. OTHER node →
                        // cache under key `G` for the ESP-NOW relay; OUR OWN id → gw_own.io for
                        // self-apply. Verbatim bytes; the node's `io::apply_wire` validates + binds
                        // (unknown type / reserved pin rejected, #46 clamp). Body is `io`-gated
                        // (the arm CONDITION matches the cast-arm precedent above — a feature's
                        // topic string in the always-compiled match, io-specific work cfg-gated).
                        #[cfg(feature = "io")]
                        {
                            let leaf_id = _leaf_id;
                            if leaf_id != node_id {
                                if let Some(cache) = cfg_cache.as_deref_mut() {
                                    let now = Instant::now().duration_since_epoch().as_millis();
                                    cache.set(leaf_id, CFG_KEY_IO, payload, [0u8; 6], now);
                                    log::info!(
                                        "smol #72: cached leaf id{} io-map for relay ({} B)",
                                        leaf_id,
                                        payload.len()
                                    );
                                }
                            } else {
                                gw_own.io = Some(GwOwnCfg::val(payload));
                                log::info!("smol #72: gateway own io-map captured ({} B)", payload.len());
                            }
                        }
                    } else if let Some(_leaf_id) = parse_leaf_config_topic(topic, b"/io/set") {
                        // #72 IO output states: twin of the config/io arm. OTHER node → cache under
                        // key `g` for the ESP-NOW relay; OUR OWN id → gw_own.io_set for self-apply.
                        // Verbatim bytes; `io::apply_set` drives the named OUTPUT slots (no-op on an
                        // unbound / input slot). Body `io`-gated (arm matches the cast-arm precedent).
                        #[cfg(feature = "io")]
                        {
                            let leaf_id = _leaf_id;
                            if leaf_id != node_id {
                                if let Some(cache) = cfg_cache.as_deref_mut() {
                                    let now = Instant::now().duration_since_epoch().as_millis();
                                    cache.set(leaf_id, CFG_KEY_IO_SET, payload, [0u8; 6], now);
                                    log::info!(
                                        "smol #72: cached leaf id{} io-set for relay ({} B)",
                                        leaf_id,
                                        payload.len()
                                    );
                                }
                            } else {
                                gw_own.io_set = Some(GwOwnCfg::val(payload));
                                log::info!("smol #72: gateway own io-set captured ({} B)", payload.len());
                            }
                        }
                    } else if let Some(leaf_id) = parse_leaf_config_topic(topic, b"/cmd/reset") {
                        // #52 remote reboot — a COMMAND on a TRANSIENT topic (retain:false), so we
                        // only see it when HA publishes DURING this session. NEVER cache it (a
                        // cached/rebroadcast reboot = a permanent ~10 s reboot-loop soft-brick):
                        // capture into `reset_req` for a ONE-SHOT relay after the burst. The topic
                        // IS the command — any payload (incl. empty) triggers. OWN id → self-reboot
                        // (routed through our CfgTracker below → the boot-debounced apply in main).
                        if leaf_id == node_id {
                            reset_req.set_own();
                            log::info!("smol #52: OWN reset command — self-reboot after burst");
                        } else {
                            reset_req.push_leaf(leaf_id);
                            log::info!("smol #52: leaf id{} reset command — one-shot relay queued", leaf_id);
                        }
                    } else if let Some(leaf_id) = parse_leaf_config_topic(topic, b"/cmd/scan") {
                        // #71 on-demand WiFi scan — a COMMAND on a TRANSIENT topic (retain:false),
                        // twin of the reset arm above. NEVER cached (a cached scan = periodic
                        // off-channel excursion, the #71 coexist hazard): capture into `scan_req`
                        // for a ONE-SHOT `<id>W` relay after the burst. The topic IS the command
                        // (any payload triggers). OWN id → self-scan (routed through our CfgTracker
                        // → main's take_cfg_offer(W) → run_scan → publish smol/<id>/scan).
                        if leaf_id == node_id {
                            scan_req.set_own();
                            log::info!("smol #71: OWN scan command — self-scan after burst");
                        } else {
                            scan_req.push_leaf(leaf_id);
                            log::info!("smol #71: leaf id{} scan command — one-shot relay queued", leaf_id);
                        }
                    } else if let Some(leaf_id) = parse_leaf_install_topic(topic) {
                        // #40 (§B3): a wildcard-delivered leaf OTA install command. On a
                        // GATEWAY flush (cfg_cache = Some), an `INSTALL` for a leaf id ≠ self
                        // arms a mesh-OTA relay to that leaf (paired with `raw_staged` after
                        // the drain). PANIC-FREE exact byte compare (untrusted retained). NOTE:
                        // the retained cmd is CLEARED only AFTER a successful pair (below), NOT
                        // here — else an install drained in a session with no staged image
                        // (a drain-timing miss) would be consumed+lost without ever arming.
                        if leaf_id != node_id && cfg_cache.is_some() && payload == b"INSTALL" {
                            // Surface this leaf's install (last-wins among concurrent retained
                            // installs). Round-robin fairness across >1 leaf was REVERTED out of
                            // this critical path (it rotated the surfaced leaf each burst, halving
                            // the audible leaf's service rate → slow-arm); it rides the #68 pass.
                            pending_leaf = Some(leaf_id);
                            *leaf_install_seen = true; // #40 #1: install-SEEN (pre-arm) → gateway suppresses its own self-OTA
                            // #111: record it in the armed set (deduped) for the version-flip cleanup.
                            if armed_n < armed_installs.len() && !armed_installs[..armed_n].contains(&leaf_id) {
                                armed_installs[armed_n] = leaf_id;
                                armed_n += 1;
                            }
                            log::info!("smol #40: leaf id{} OTA install command received", leaf_id);
                        }
                    }
                    // #101: a QoS1 command (cmd/reset / cmd/scan) carries a packet id → PUBACK it
                    // below (after the acc compaction) so the broker drops the one-shot command.
                    (consumed, packet_id)
                }
                Some((_, consumed)) => (consumed, None),
            };
            // #40 QUIET-PERIOD: a packet was parsed (the `None` arm broke above) → reset the
            // quiet timer so an in-flight retained burst keeps the drain alive.
            last_msg = Instant::now();
            acc.copy_within(consumed..acc_len, 0);
            acc_len -= consumed;
            // #101: PUBACK a delivered QoS1 command so the broker drops it from our (now persistent)
            // session — else it redelivers on every reconnect → reboot loop (the soft-brick the
            // retain:false choice avoided). Sent AFTER the compaction so the `acc` borrow is
            // released; reuses the same pkt/tcp_send/deadline/tick handles as the SUBSCRIBEs above.
            // ACK-THEN-ACT egress guarantee (the crux): this PUBACK is pushed to the wire HERE
            // (tcp_send + this loop's iface.poll), and the reset never fires in-handler — the only
            // software_reset() is in main, AFTER flush_telemetry returns, i.e. AFTER the GRACEFUL
            // DISCONNECT + `socket.close()` below (smoltcp close() = FIN with the TX buffer DRAINED,
            // NOT abort()/RST that would discard a still-buffered PUBACK). Even an operator
            // long-press abort BREAKS to that same clean DISCONNECT. So the ack always egresses
            // before the post-flush reboot → the redelivery loop physically cannot occur.
            if let Some(pid) = puback_id {
                if let Some(n) = crate::net::mqtt::encode_puback(&mut pkt, pid) {
                    let _ = tcp_send(iface, device, sockets, tcp_handle, &pkt[..n], deadline, tick);
                }
            }
        }
        // #40 QUIET-PERIOD break: exit once the 3 downlink flags are in AND no new packet has
        // arrived for DOWNLINK_SETTLE. The retained backlog (staged/install/cfg) arrives as a
        // contiguous SUBACK burst, so a quiet gap means it's fully drained — order-independent,
        // unlike the old fixed window that truncated a late-arriving OTA topic. If batt/grid/mc
        // never all arrive (e.g. absent MC at boot), fall through to the `deadline` break — which
        // still drains ota/config during the wait (the original boot behaviour).
        if got_batt && got_grid && got_mc && Instant::now() >= last_msg + DOWNLINK_SETTLE {
            break;
        }
        if Instant::now() > deadline {
            log::info!("smol: MQTT downlink(s) not all received in budget (keeping cache)");
            break;
        }
    }

    // #40 ARMDIAG: snapshot whether THIS flush caught an install (before the diag block below
    // may null `pending_leaf`) — dumped to `smol/<gw>/ota/armdiag` after the arm, so one
    // reflash shows EXACTLY which arm-chain link is null (headless arm trace).
    let install_caught = pending_leaf;

    // #40 DIAG + clear/retry (from the PRIOR attempt, recorded by `main` after the relay):
    // publish the phase to retained `smol/<leaf>/ota/diag` (headless observability — the
    // mesh-only leaf has no serial), then either CLEAR the retained install (terminal phase
    // = the leaf installed/rolled-back, or the transient-retry cap was hit) or LEAVE it
    // retained to retry. On a clear, also null `pending_leaf` for THIS flush so we don't
    // re-arm a just-finished leaf. Runs BEFORE the arm below. Consumed after publish.
    if let Some((lid, phase, clear, retry)) = *leaf_diag {
        let mut dtopic = MqttScratch::new();
        let _ = write!(dtopic, "smol/{}/ota/diag", lid);
        // #134: surface the consecutive-failure count in the retained payload ("fetch-failed
        // retry=3") so a stuck fetch is VISIBLE headlessly instead of silently burning the order.
        // retry == 0 (a terminal / first attempt) → just the bare phase, unchanged from before.
        let mut dpayload = MqttScratch::new();
        if retry > 0 {
            let _ = write!(dpayload, "{} retry={}", phase, retry);
        } else {
            let _ = write!(dpayload, "{}", phase);
        }
        if let Some(n) =
            crate::net::mqtt::encode_publish(&mut pkt, dtopic.as_bytes(), dpayload.as_bytes(), true)
        {
            let _ = tcp_send(iface, device, sockets, tcp_handle, &pkt[..n], deadline, tick);
        }
        log::info!("smol #40: published smol/{}/ota/diag = {} retry={} (clear={})", lid, phase, retry, clear);
        if clear {
            let mut itopic = MqttScratch::new();
            let _ = write!(itopic, "smol/{}/ota/install", lid);
            if let Some(n) = crate::net::mqtt::encode_publish(&mut pkt, itopic.as_bytes(), b"", true) {
                let _ = tcp_send(iface, device, sockets, tcp_handle, &pkt[..n], deadline, tick);
            }
            if pending_leaf == Some(lid) {
                pending_leaf = None; // don't re-arm a leaf we just consumed
            }
        }
        *leaf_diag = None; // consumed — published once
    }

    // #139-followup: publish this node's OWN failed-self-fetch record to retained
    // `smol/<id>/ota/diag` (#135 armdiag pattern). Release images are serial-silent, so a self-OTA
    // that dies mid-download is otherwise INVISIBLE on the fleet — the blindness that turned
    // tonight's mid-body deaths into a three-hour diagnosis. `chunk=k/n` = how far the download got
    // (0/0 ⇒ died before the download: assoc/DHCP/slot); `retry`/`stall` = transfer trouble.
    // Consumed after publish (set None) so it isn't re-emitted every flush.
    if let Some((k, n, r, s, w)) = *ota_self_fail {
        let mut dtopic = MqttScratch::new();
        let _ = write!(dtopic, "smol/{}/ota/diag", node_id);
        let mut dval = MqttScratch::new();
        // #147: `at=<label>` names the exact stage that died (handshake / recv / verify / …) so a
        // failure is self-describing on the wire — release images are serial-silent.
        let _ = write!(
            dval,
            "self-fetch-failed chunk={}/{} retry={} stall={} at={}",
            k, n, r, s, ota_fail::label(w)
        );
        if let Some(np) =
            crate::net::mqtt::encode_publish(&mut pkt, dtopic.as_bytes(), dval.as_bytes(), true)
        {
            let _ = tcp_send(iface, device, sockets, tcp_handle, &pkt[..np], deadline, tick);
        }
        log::info!(
            "smol #139/#147: published smol/{}/ota/diag = self-fetch-failed chunk={}/{} retry={} stall={} at={}",
            node_id, k, n, r, s, ota_fail::label(w)
        );
        *ota_self_fail = None; // consumed — published once
    }

    // #111: CROWN-PORTABLE install cleanup — clear a leaf's retained `ota/install` once its FRESH
    // STAT confirms the version-flip (reported build >= the staged build). This is the SUCCESS
    // clear that survives a crown handover: the old design cleared only on the RELAY outcome
    // (`record_leaf_ota`, in-RAM, dies with the gateway tenure), so an install caught by one crown
    // and completed under another was never cleared here — it re-armed forever OR (worse, paired
    // with the eager self-clear) evaporated. Now ANY crown that sees the flip clears it. The
    // freshness gate (`entry_fresh`) IS the "seen a STAT since staging" guard: a stale/absent STAT
    // → unknown build → left retained (unknown != completed). A fresh-but-old STAT has build <
    // staged → also left (not flipped yet). Only armed ids (seen via the wildcard this burst) are
    // considered, ≤1 clear/burst (extras re-clear next burst). `record_leaf_ota`'s retry-cap /
    // rolled-back clear stays as the DOOMED-install backstop (a rollback never version-flips).
    if let (Some(sc), Some(staged)) = (stat_cache, *staged_raw) {
        let now_ms = Instant::now().duration_since_epoch().as_millis();
        'flip: for i in 0..sc.count() {
            let Some((lid, val)) = sc.entry_fresh(i, now_ms, STAT_FRESH_MS) else {
                continue;
            };
            if lid == node_id || !armed_installs[..armed_n].contains(&lid) {
                continue;
            }
            // Build = the last '|'-segment of the STAT value (mirrors `ota_mesh::stat_build`,
            // inlined so this wifi-tier path carries no espnow-only dependency).
            let flipped = core::str::from_utf8(val)
                .ok()
                .and_then(|s| s.rsplit('|').next())
                .and_then(|b| b.parse::<u32>().ok())
                .is_some_and(|b| b >= staged.build);
            if flipped {
                let mut itopic = MqttScratch::new();
                let _ = write!(itopic, "smol/{}/ota/install", lid);
                if let Some(n) = crate::net::mqtt::encode_publish(&mut pkt, itopic.as_bytes(), b"", true) {
                    let _ = tcp_send(iface, device, sockets, tcp_handle, &pkt[..n], deadline, tick);
                }
                log::info!(
                    "smol #111: leaf id{} version-flipped (>= staged {}) — cleared retained install",
                    lid, staged.build
                );
                if pending_leaf == Some(lid) {
                    pending_leaf = None; // don't re-arm a leaf that already completed
                }
                break 'flip; // ≤1 clear/burst
            }
        }
    }

    // #3 RELAY RX-DIAG: publish the last relay attempt's RX evidence to retained
    // `smol/<leaf>/ota/relaydiag` (headless #3 disambiguation). rx=0 → gateway heard NOTHING
    // from the leaf (leaf offline / OTAD not landing); rx>0 & otan=0 → leaf alive but never
    // NAK'd this session; otan>0 & last_wb<total → chunk-loss stall. Consumed after publish.
    if let Some(d) = *leaf_relay_rx {
        let mut rtopic = MqttScratch::new();
        let _ = write!(rtopic, "smol/{}/ota/relaydiag", d.leaf_id);
        let mut rval = MqttScratch::new();
        // Gateway RX evidence + the leaf's own LDBG self-report. `leaf=none` ⇒ no LDBG captured
        // (old leaf fw / leaf off-air during the relay); else `H<heard>V<verdict>N<sent>`.
        let _ = write!(rval, "rx={} otan={} last_wb={}/{} otam_tx={}/{} settle={} leaf=",
            d.rx_any, d.otan_valid, d.last_wb, d.total, d.otam_tx, d.otam_ok, d.settle);
        if d.leaf_verdict == 255 {
            let _ = write!(rval, "none");
        } else {
            let _ = write!(rval, "H{}V{}N{}ch{}", d.leaf_heard, d.leaf_verdict, d.leaf_sent, d.leaf_ch);
        }
        if let Some(n) =
            crate::net::mqtt::encode_publish(&mut pkt, rtopic.as_bytes(), rval.as_bytes(), true)
        {
            let _ = tcp_send(iface, device, sockets, tcp_handle, &pkt[..n], deadline, tick);
        }
        log::info!(
            "smol #3: relaydiag id{} rx={} otan={} last_wb={}/{} leafV={}",
            d.leaf_id, d.rx_any, d.otan_valid, d.last_wb, d.total, d.leaf_verdict
        );
        *leaf_relay_rx = None; // consumed — published once
    }

    // #40: ARM a pending leaf install by pairing it with the (persisted) staged image. Does
    // NOT clear the install here — the clear is OUTCOME-driven (the diag block above, once
    // `main` reports the relay result), so a mac-unknown / fetch-fail / relay-fail LEAVES the
    // install retained → the next flush retries (bounded by LEAF_OTA_MAX_RETRIES). `Announce`
    // is `Copy`, so the pair moves out cleanly.
    match (pending_leaf, *staged_raw) {
        (Some(leaf_id), Some(ann)) => {
            *leaf_ota = Some((leaf_id, ann));
            log::info!("smol #40: ARMED relay for leaf id{} (staged build {})", leaf_id, ann.build);
        }
        (Some(leaf_id), None) => {
            log::warn!(
                "smol #40: leaf id{} install pending but NO staged image known yet — leaving retained for the next flush to arm",
                leaf_id
            );
        }
        _ => {}
    }

    // #40 ARMDIAG — dump the arm-chain state EVERY gateway flush to retained
    // `smol/<gw>/ota/armdiag`, so ONE reflash shows exactly which link is null (the C3 gives
    // no serial). `install-caught` = an INSTALL for a leaf hit this flush; `staged_raw` = the
    // persisted staged build; `leaf_ota` = the pair armed. If install-caught=<id> +
    // staged_raw=<b> + leaf_ota=1 → armed (issue is downstream, in the relay). If
    // staged_raw=none → the persist path never wrote it. If install-caught=none → the wildcard
    // sub / arm never fired. Gateway-only (cfg_cache = Some).
    if cfg_cache.is_some() {
        let mut adtopic = MqttScratch::new();
        let _ = write!(adtopic, "smol/{}/ota/armdiag", node_id);
        let mut adval = MqttScratch::new();
        let _ = write!(adval, "install-caught=");
        match install_caught {
            Some(x) => { let _ = write!(adval, "{}", x); }
            None => { let _ = write!(adval, "none"); }
        }
        let _ = write!(adval, " pending=");
        match pending_leaf {
            Some(x) => { let _ = write!(adval, "{}", x); }
            None => { let _ = write!(adval, "none"); }
        }
        let _ = write!(adval, " staged_raw=");
        match staged_raw.as_ref() {
            Some(a) => { let _ = write!(adval, "{}", a.build); }
            None => { let _ = write!(adval, "none"); }
        }
        let _ = write!(adval, " leaf_ota={}", if leaf_ota.is_some() { 1 } else { 0 });
        if let Some(n) = crate::net::mqtt::encode_publish(&mut pkt, adtopic.as_bytes(), adval.as_bytes(), true) {
            let _ = tcp_send(iface, device, sockets, tcp_handle, &pkt[..n], deadline, tick);
        }
    }

    // --- #23 single-gateway ELECTION with RUNTIME re-decision + stale-owner takeover ---
    // (Fixes oracle #1 dead-owner wedge + #2 split-brain: the decision here flows BACK to
    // the live role via `elect`, and a frozen `seq` lets a dead owner be taken over.)
    //
    // 1. Refresh the persistent staleness observation: a *changed* (owner,seq) resets the
    //    "first seen" clock (fresh liveness); an unchanged pair keeps it (staleness accrues).
    // 2. TWO election rules, selected by `elect.recovery`:
    //    * boot / gateway-flush (`recovery == false`): the ORIGINAL, hardware-validated
    //      lowest-id election — adopt a LIVE lower-id owner, else claim. UNCHANGED, so
    //      cold-start stays fast + #2 split-brain resolution stays as-verified.
    //    * #51 LEAF RECOVERY (`recovery == true`): WiFi-strength election — NEVER override a
    //      LIVE owner (sticky), and take over a DEAD owner only after this board's
    //      RSSI-weighted backoff, so the strongest-uplink survivor wins (node-id breaks
    //      ties). `channel` stays 0 (advisory; leaves discover it by scanning the HELLO).
    // NB: the "same owner we're waiting out" vs "a live owner to grace" distinction is now made
    // by `reset` (owner-changed OR seq-advanced this burst — see the alive branch + #114 churn
    // residual note), so no separate pre-observation snapshot of `seen_owner` is needed here.
    let claim_seq: Option<u32> = match mc {
        Some((owner, _ch, seq)) => {
            // Refresh the staleness observation: a *changed* (owner,seq) resets the first-seen
            // clock. Same rule on BOTH paths — an ADVANCING seq is the authoritative
            // broker-liveness signal (a dead board can't publish), so it MUST reset "alive"
            // even in recovery. That's the split-brain guard: an owner we lost on the mesh
            // (HELLO) but is still flushing MC (seq climbing) is ALIVE → we adopt, never take
            // over. Recovery differs ONLY in the shorter `RECOVERY_STALE_MS` window (a frozen
            // seq is confidently dead sooner, because HELLO-silence already corroborates).
            let reset = owner != elect.seen_owner || seq != elect.seen_seq;
            if reset {
                elect.seen_ms = elect.now_ms;
            }
            elect.seen_owner = owner;
            elect.seen_seq = seq;
            // #114 H3 (stale-self-reclaim fix): choose the dead-owner window by CORROBORATION.
            // `owner_never_heard` was introduced by H1 to mean "a forged/phantom retained MC no
            // board ever heard — take it over promptly". But it is ALSO true for EVERY freshly
            // booted (or freshly roamed) leaf in its first seconds of life: it hasn't had a chance
            // to hear the LIVE owner's HELLO yet. So a short window here let a just-booted board
            // seize the crown from a healthy, actively-flushing owner it simply hadn't heard —
            // the H3 sighting (`MC|<self>|0|seq+N` over a live foreign owner). The seq is the only
            // signal that disambiguates a phantom from a not-yet-heard live owner: a live gateway
            // flushes MC every <=RELAY_FLUSH_INTERVAL_MS (30s) so its seq advances (→ our anchor
            // resets → `alive`), while a phantom's seq is frozen forever. So require the CONSERVATIVE
            // MC_STALE_MS (90s = 3 missed flushes) freeze before taking over an owner we've NEVER
            // heard: a live owner provably bumps its seq ~3x inside that window and is adopted, and
            // only a genuinely-dead phantom survives to be taken over (still heals the H1 standoff,
            // just at 90s instead of 35s — election stability > speed). A HEARD-then-lost owner keeps
            // the fast RECOVERY_STALE_MS (35s): the owner-HELLO silence that opened this recovery is
            // independent corroboration of death, so no live board can be misjudged there.
            // #136: the HEARD-then-lost window is floored at the worst-case gap between a LIVE
            // owner's OBSERVED seq advances (`recovery_stale_floor_ms` = flush interval + a slow
            // flush bounded by RELAY_FLUSH_BUDGET, seeded by the caller). At today's 30 s flush that
            // lifts the effective window 35→45 s so a budget-edge re-assoc flush (seq merely delayed,
            // not frozen) can't cross it — the owner advances its seq and is adopted, never taken
            // over (#136 canary 1). A genuinely-dead owner's seq is frozen forever, so it is still
            // taken over at the window (#136 canary 2). Tracks #122 B1 automatically (at F=20 the
            // floor is 20+15=35, already covered). NEVER-heard keeps MC_STALE_MS (3×F, ≥ the floor).
            let stale_limit = if elect.recovery {
                if elect.owner_never_heard {
                    MC_STALE_MS
                } else {
                    RECOVERY_STALE_MS.max(elect.recovery_stale_floor_ms)
                }
            } else {
                MC_STALE_MS
            };
            let alive = elect.now_ms.saturating_sub(elect.seen_ms) < stale_limit;
            if elect.boot && owner != node_id {
                // #51 return-flap fix: a FRESH-booting board never displaces a DIFFERENT owner
                // already in the retained MC. Come up as a leaf and defer — leaf-scan locks a
                // LIVE owner's HELLO in seconds (no re-claim bounce), and the recovery election
                // takes over a genuinely DEAD one after its window. (At boot we have only ONE
                // MC sample so we can't tell live from dead here; deferring lets the fast HELLO
                // path decide. The COMMON cold-boot has no delay: the prior gateway reads
                // owner==self below and re-claims its own record immediately; only peers defer.)
                elect.owner_alive = alive;
                elect.i_am_owner = false;
                elect.owner_id = owner;
                None
            } else if !elect.recovery {
                elect.owner_alive = alive;
                // BOOT (empty/own MC) / gateway-flush — original lowest-id rule (as verified).
                if owner < node_id && alive {
                    elect.i_am_owner = false;
                    elect.owner_id = owner;
                    None
                } else {
                    // id >= mine, or a STALE (dead) lower-id owner → claim / take over.
                    elect.i_am_owner = true;
                    elect.owner_id = node_id;
                    Some(seq.wrapping_add(1))
                }
            } else if owner == node_id {
                // #51 recovery: our own retained record → hold ownership.
                elect.owner_alive = false;
                elect.i_am_owner = true;
                elect.owner_id = node_id;
                Some(seq.wrapping_add(1))
            } else if alive {
                // #51 recovery: a LIVE owner (any id) is sticky — never override it. This is
                // what makes the strongest board, once it claims, stay owner as the weaker
                // survivors observe its fresh MC and adopt it.
                //
                // #114 churn residual (issue #122, item 2): `owner_alive` grace-resets the caller's
                // owner-silence clock (mode.rs). It was `owner != lost_owner` (a DIFFERENT owner
                // only), so a leaf re-adopting the SAME live owner NEVER reset the clock → it
                // re-elected every REELECT_RETRY_MS forever, and each recovery burst re-associates
                // OFF the ESP-NOW channel, starving its own HELLO lock (owner_hello_seen stays
                // false) — the perpetual recovery-burst churn. Fix: reset on FRESH liveness
                // evidence = `reset` (a DIFFERENT owner — a successor to follow — OR the SAME owner
                // whose seq ADVANCED this burst, i.e. it published a new MC since our last read →
                // provably alive NOW). Each genuine seq advance pauses the redundant bursts, giving
                // the leaf on-channel time to lock the owner's HELLO, which then ends recovery.
                //
                // NO liveness blind spot — this SUPERSEDES the old "first-burst baseline" worry:
                // `owner_alive`/`last_owner_heard_ms` gates only WHEN recovery bursts FIRE, NOT the
                // takeover ANCHOR. The takeover window is measured from `seen_ms`, which the `reset`
                // arm above sets INDEPENDENTLY of `owner_alive`. So a genuinely-dead owner (seq
                // frozen ⇒ reset=false after at most ONE first-burst catch-up) still accrues to
                // takeover on `seen_ms` and is taken over on schedule; a same-owner reset only ever
                // pauses bursts by < REELECT_SILENCE_MS (15s) — far inside the stale window — so it
                // has ZERO net effect on failover timing. A live owner (seq advancing) is correctly
                // never taken over. The dead-but-inside-backoff path (the `else` arm) still sets
                // owner_alive=false, so a deferred takeover keeps firing on cadence, unchanged.
                elect.owner_alive = reset;
                elect.i_am_owner = false;
                elect.owner_id = owner;
                None
            } else {
                // #51 recovery: owner is DEAD (its seq stayed frozen past `stale_limit` — 35s if
                // we HEARD-then-lost it, 90s if we NEVER heard it; see the H3 note above). Take
                // over once past that window PLUS our stagger backoff — the best survivor crosses
                // first; the others read its fresh MC and adopt it.
                elect.owner_alive = false;
                // #114 H1: a dead owner this leaf NEVER heard a HELLO from is a forged / phantom
                // retained MC (the standoff) confirmed dead by the conservative 90s freeze above —
                // there is no live board to RSSI-stagger against, so use a small id-only tiebreak
                // (lowest id first) rather than the RSSI ladder. A heard-then-died owner keeps the
                // RSSI-weighted stagger (pick the best survivor).
                let backoff = if elect.owner_never_heard {
                    (node_id as u64) * 200
                } else {
                    reelect_backoff_ms(elect.my_rssi, node_id)
                };
                // #114 H3: gate on `stale_limit` (not a hardcoded RECOVERY_STALE_MS) so the
                // never-heard takeover honours the SAME conservative 90s window used for `alive`
                // above — otherwise a never-heard owner could re-enter here and claim at 35s.
                if elect.now_ms.saturating_sub(elect.seen_ms)
                    >= stale_limit.saturating_add(backoff)
                {
                    elect.i_am_owner = true;
                    elect.owner_id = node_id;
                    Some(seq.wrapping_add(1))
                } else {
                    // Dead, but still inside OUR backoff → defer so a stronger board claims
                    // first. owner_alive already false → caller keeps the silence anchor.
                    elect.i_am_owner = false;
                    elect.owner_id = owner;
                    None
                }
            }
        }
        None => {
            // Empty/absent/unparseable topic → claim immediately (fast cold-start; a recovery
            // that finds the record cleared also claims — sticky-adopt converges any race).
            elect.owner_alive = false;
            elect.i_am_owner = true;
            elect.owner_id = node_id;
            Some(1)
        }
    };
    // #146 CLAIM guard: a board that abdicated on sustained flush failure (its WiFi uplink is
    // PROVEN unable to complete a broker flush) must participate as LEAF ONLY — it may adopt an
    // owner but must never (re)claim the crown, not even its own stale retained `MC`. Suppress the
    // claim HERE, before any MC publish, so the retained record is left to FREEZE and the H1/H2
    // takeover machinery elects a flush-capable board off that frozen seq. `i_am_owner == true`
    // iff `claim_seq.is_some()` on every arm above, so clearing it here is exactly the claim set.
    // Composes with #137: a live, actively-FLUSHING owner is never latched (its flushes succeed →
    // the latch is clear), so this is a no-op for a healthy fleet — zero behavior change.
    let claim_seq = if elect.flush_incapable {
        if claim_seq.is_some() {
            elect.i_am_owner = false;
            log::warn!(
                "smol: #146 election — flush-incapable, refusing to claim (leaf-only until a flush succeeds)"
            );
        }
        None
    } else {
        claim_seq
    };
    // #155 channel-drag OPERATOR LEVER: honor the retained `smol/mesh/channel_hint`. The mesh
    // channel is PHYSICALLY the crown's AP channel (coexist single-radio), so a hint can only steer
    // WHICH board holds the crown: a candidate whose OWN channel is KNOWN (non-zero) and != the
    // hint must not claim, so the (re)election converges onto a board already on the hinted channel
    // and the fleet stops being dragged onto a weak AP. Same claim-guard SHAPE as #146 above —
    // suppress the claim HERE, before the MC publish, so the retained record is left to freeze and
    // a hinted-channel board takes over off that frozen seq. `hint_blocked` tells the caller
    // (mode.rs flush) to go HELLO-silent on a hint-driven yield so a SITTING crown vacates promptly.
    // FAIL-OPEN on an unknown own-channel (`my_channel == 0`): a not-yet-learned board claims as
    // before (never a crownless mesh while a channel is still being learned). No hint ⇒ no-op.
    let claim_seq = match (claim_seq, elect.channel_hint) {
        (Some(_), Some(h)) if elect.my_channel != 0 && elect.my_channel != h => {
            elect.i_am_owner = false;
            elect.hint_blocked = true;
            log::info!(
                "smol: #155 channel_hint ch{} != my ch{} — yielding crown (leaf until on-hint)",
                h,
                elect.my_channel
            );
            None
        }
        (cs, _) => cs,
    };
    if let Some(newseq) = claim_seq {
        // Record my own ownership locally so my seq counts as "fresh" next read, then
        // publish the retained record (the liveness heartbeat other boards watch).
        elect.seen_owner = node_id;
        elect.seen_seq = newseq;
        elect.seen_ms = elect.now_ms;
        let mut mcp = MqttScratch::new();
        let _ = write!(mcp, "MC|{}|{}|{}", node_id, elect.my_channel, newseq); // #29: real ch (0 until learned)
        // #51 A2 — CLAIM-AFTER-PUBLISH: only actually hold ownership if the retained MC
        // write reached the broker (proof our uplink is alive). If the publish fails, we
        // are NOT a valid owner — revert to leaf so ownership can't land on a board whose
        // broker link just died (prevents a dead-uplink board re-pinning the mesh).
        let published = match crate::net::mqtt::encode_publish(
            &mut pkt,
            MESH_CHANNEL_TOPIC,
            mcp.as_bytes(),
            true,
        ) {
            Some(n) => tcp_send(iface, device, sockets, tcp_handle, &pkt[..n], deadline, tick),
            None => false,
        };
        if !published {
            elect.i_am_owner = false; // couldn't prove uplink → stay leaf, retry next burst
            log::warn!("smol: mesh election — MC publish FAILED, not claiming ownership");
        }
        // #114 H2: seq-claim RACE tie-break. Two candidates that hit an EMPTY MC simultaneously both
        // claim seq=1 (last-write-wins on the broker) and both would burst as gateway — a split-brain
        // that today only reconverges via the lowest-id flush resolver (~1–2 flushes). After OUR claim
        // publish, briefly re-read the retained MC: a DIFFERENT owner appearing means a concurrent
        // claim landed → resolve DETERMINISTICALLY by lowest id (the HIGHER id always YIELDS; a lower
        // id re-asserts seq+1 so the retained record names it). Bounded (≤400 ms, tick-abortable) +
        // FAIL-SAFE: a timeout / parse-miss keeps our original verdict (never a regression), and we
        // never yield to — nor claim over — an owner we haven't out-ranked by id.
        if elect.i_am_owner && published {
            let reread_deadline = Instant::now() + Duration::from_millis(400);
            let mut competitor: Option<(u8, u8, u32)> = None;
            while Instant::now() < reread_deadline {
                if tick() {
                    break;
                }
                iface.poll(smoltcp_now(), device, sockets);
                recv_into(sockets, tcp_handle, &mut acc[..], &mut acc_len);
                loop {
                    match crate::net::mqtt::parse_packet(&acc[..acc_len]) {
                        Some((crate::net::mqtt::Incoming::Publish { topic, payload, .. }, consumed)) => {
                            if topic == MESH_CHANNEL_TOPIC {
                                if let Some(mc2) = parse_mesh_channel(payload) {
                                    competitor = Some(mc2);
                                }
                            }
                            acc.copy_within(consumed..acc_len, 0);
                            acc_len -= consumed;
                        }
                        Some((_, consumed)) => {
                            acc.copy_within(consumed..acc_len, 0);
                            acc_len -= consumed;
                        }
                        None => break,
                    }
                }
                // Stop as soon as we've seen a competing owner (a concurrent claim landed).
                if competitor.map(|(o, _, _)| o != node_id).unwrap_or(false) {
                    break;
                }
            }
            if let Some((owner2, _ch2, seq2)) = competitor {
                if owner2 > node_id {
                    // A HIGHER id overwrote our claim, but we out-rank it → re-assert with seq+1 so
                    // the retained record names us (the lowest); it yields when it re-reads.
                    let reseq = seq2.wrapping_add(1);
                    let mut mcp2 = MqttScratch::new();
                    let _ = write!(mcp2, "MC|{}|{}|{}", node_id, elect.my_channel, reseq);
                    if let Some(n) =
                        crate::net::mqtt::encode_publish(&mut pkt, MESH_CHANNEL_TOPIC, mcp2.as_bytes(), true)
                    {
                        if tcp_send(iface, device, sockets, tcp_handle, &pkt[..n], deadline, tick) {
                            elect.seen_seq = reseq;
                            log::info!("smol: #114 H2 — re-asserted claim over higher-id {} (seq {})", owner2, reseq);
                        }
                    }
                } else if owner2 < node_id {
                    // A LOWER id also claimed and holds the slot → YIELD (no duplicate gateway).
                    elect.i_am_owner = false;
                    elect.owner_id = owner2;
                    elect.seen_owner = owner2;
                    elect.seen_seq = seq2;
                    elect.seen_ms = elect.now_ms;
                    log::info!("smol: #114 H2 — yielding to lower-id owner {} (claim race)", owner2);
                }
            }
        }
    }
    log::info!(
        "smol: mesh election -> {} (owner id{}, seen seq {})",
        if elect.i_am_owner { "OWNER/gateway" } else { "leaf" },
        elect.owner_id,
        elect.seen_seq
    );

    // #76 RETAINED-PEERS GHOST FIX (also closes the #68 retained-peers-cleanup item): the peers
    // publish above (retained `smol/<id>/peers`) only fires while this board is the OWNER. When it
    // DEMOTES to leaf it stops flushing, so its last `PEERS|G|…` persists on the broker forever as
    // a GHOST → observability shows a phantom extra gateway. This was the bulk of the #40-sweep
    // "election split-brain" (#76): the all-three-PEERS|G was largely these stale retained records,
    // not a live triple-claim (the election itself reconverged to a single owner post-bounce). So
    // the moment the election says we are NOT the owner, CLEAR our retained peers with an empty
    // payload (the board is still connected here — same free window the HA block below uses).
    // Idempotent: a steady leaf just re-clears an already-empty topic. Same retained-hygiene class
    // as F6 / stat_cache / armdiag — a demoted role must not leave a live-looking retained trace.
    if !elect.i_am_owner {
        let mut ptopic = MqttScratch::new();
        let _ = write!(ptopic, "smol/{}/peers", node_id);
        if let Some(n) = crate::net::mqtt::encode_publish(&mut pkt, ptopic.as_bytes(), b"", true) {
            let _ = tcp_send(iface, device, sockets, tcp_handle, &pkt[..n], deadline, tick);
        }
    }

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
            // #39: `retain:true` → HA publishes update.install (native tile + Lovelace update-action)
            // to cmd_t RETAINED, so the node catches it on its next ~2 s burst (the fw acts ONLY on a
            // retained install; a non-retained one is missed between bursts). Unifies Install into the
            // native Update tile and lets the HA-side retained install buttons retire.
            "{{\"~\":\"smol/{}/ota\",\"stat_t\":\"~/state\",\"cmd_t\":\"~/install\",\"pl_inst\":\"INSTALL\",\"retain\":true,\"dev_cla\":\"firmware\",\"name\":\"Update\",\"has_entity_name\":true,\"uniq_id\":\"smol{}_update\",\"object_id\":\"smol_{}_update\",\"dev\":{{\"ids\":[\"smol{}\"]}}}}",
            node_id, node_id, node_id, node_id
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
        // #111: clear our OWN retained install ONLY once the staged image is KNOWN (parsed this
        // session) — not merely because we caught the INSTALL. The old eager clear-on-catch lost the
        // order in the boot-race: a rebooting board caught its own retained INSTALL BEFORE it had
        // drained `ota/staged`, so `staged_raw` was None, `main` had nothing to fetch, yet the order
        // was cleared anyway (id7, 13:56). This stays a ONE-SHOT clear (fires at the install, not
        // after a version-flip), which is deliberate: the OWN self-OTA gate is `build > BUILD_NUMBER`
        // only (NOT floor-gated, unlike the leaf mesh path), so a version-flip-clear would re-fetch a
        // rolled-back image forever — clearing at install-time is what keeps a bad self-image one-shot.
        //
        // #147: gate the clear on `ota_offer.is_some()` (a GATE-PASSING staged target — the thing
        // `main` actually fetches via `take_ota_offer`), NOT `staged_raw.is_some()` (any staged line
        // that merely PARSED). A record that parsed but the gate REFUSED (monotonicity / host-
        // allowlist / bad size — `ota_offer` stays None) previously burned the paired install order
        // WITHOUT any fetch (an operator's bad-recovery staging killed the order). A pre-fetch refusal
        // must PRESERVE the order — the same never-clear semantics #135/#134 give pre-relay failures
        // (`reached_leaf()==false`) — so the next good staging (or the next crown) still installs.
        // `ota_offer.is_some()` ⟹ `staged_raw.is_some()`, so the boot-race guard above is unchanged.
        if *install_requested && ota_offer.is_some() {
            if let Some(n) = crate::net::mqtt::encode_publish(&mut pkt, cmd_topic.as_bytes(), &[], true) {
                let _ = tcp_send(iface, device, sockets, tcp_handle, &pkt[..n], deadline, tick);
            }
        }
    }

    // --- #40 §B2: publish each RELAYED LEAF's Update discovery + ota/state ON ITS BEHALF ---
    // A credential-less leaf never opens MQTT, so the gateway is its proxy (the same role it
    // plays for the leaf's telemetry/status relay). SAME `smol<leaf>_update` unique_id +
    // topics as self-OTA → ONE entity, device-merged onto the leaf's existing card; the
    // gateway keeps `installed_version` fresh from the RELAYED STAT build, so a leaf-mesh-OTA
    // result — the new build, or a self-test rollback to the old — shows in HA HEADLESS
    // (the whole point: the C3 USB-JTAG boards give no reliable headless serial). Gateway-only
    // (stat_cache = Some). Idempotent retained publishes, bounded to the heard-leaf set.
    #[cfg(feature = "espnow")]
    if let Some(sc) = stat_cache {
        let latest_staged = staged_raw.as_ref().map(|a| a.build);
        for i in 0..sc.count() {
            // #56: the status cache is single-channel per leaf; its key column is inert here.
            let (lid, _key, val) = match sc.entry(i) {
                Some(x) => x,
                None => continue,
            };
            if lid == node_id {
                continue; // the gateway's own Update is self-published above
            }
            let noun = crate::net::names::name_for_id(lid).1;
            // Discovery (retained) — the self-OTA template, for <leaf>. `cmd_t=~/install`
            // = `smol/<leaf>/ota/install`, which the gateway wildcard-subs → relay (§B3).
            let mut dtopic = MqttScratch::new();
            let _ = write!(dtopic, "homeassistant/update/smol{}/config", lid);
            let mut djson = MqttScratch::new();
            let _ = write!(
                djson,
                // #39: `retain:true` (see the gateway's own Update above) — HA publishes the leaf's
                // install retained → the gateway wildcard-subs it + relays (§B3), so the native tile
                // Install works for relayed leaves too, not just the retained HA-side buttons.
                "{{\"~\":\"smol/{}/ota\",\"stat_t\":\"~/state\",\"cmd_t\":\"~/install\",\"pl_inst\":\"INSTALL\",\"retain\":true,\"dev_cla\":\"firmware\",\"name\":\"Update\",\"has_entity_name\":true,\"uniq_id\":\"smol{}_update\",\"object_id\":\"smol_{}_update\",\"dev\":{{\"ids\":[\"smol{}\"],\"name\":\"smol {} {}\"}}}}",
                lid, lid, lid, lid, lid, noun
            );
            if let Some(n) = crate::net::mqtt::encode_publish(&mut pkt, dtopic.as_bytes(), djson.as_bytes(), true) {
                let _ = tcp_send(iface, device, sockets, tcp_handle, &pkt[..n], deadline, tick);
            }
            // State (retained) ONLY if the leaf reported a build (STAT `|<build>` field) —
            // don't clobber a self-published value with "unknown" for a leaf on old firmware.
            if let Some(installed) = crate::ota_mesh::stat_build(val) {
                let latest = latest_staged.unwrap_or(installed);
                let mut sjson = MqttScratch::new();
                let _ = write!(
                    sjson,
                    "{{\"installed_version\":\"{}\",\"latest_version\":\"{}\",\"in_progress\":false,\"title\":\"smol v{}\"}}",
                    installed, latest, latest
                );
                let mut stopic = MqttScratch::new();
                let _ = write!(stopic, "smol/{}/ota/state", lid);
                if let Some(n) = crate::net::mqtt::encode_publish(&mut pkt, stopic.as_bytes(), sjson.as_bytes(), true) {
                    let _ = tcp_send(iface, device, sockets, tcp_handle, &pkt[..n], deadline, tick);
                }
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
    // #48 GwOwnCfg: the gateway's own keyed configs (led/…) — forwarded to `mqtt_session`.
    gw_own: &mut GwOwnCfg,
    // #52 remote reboot: reset commands seen this burst — forwarded to `mqtt_session`.
    reset_req: &mut ResetReq,
    // #33: set true iff a retained OTA `install` command is present for this board.
    install_requested: &mut bool,
    // #40 #1: set true iff a retained leaf `install` (for another node) is seen this burst.
    leaf_install_seen: &mut bool,
    // #27: this node's serialized roster (`PEERS|…`) to publish retained as
    // `smol/<id>/peers`. Empty ⇒ nothing published (leaf / election-only burst).
    peers: &[u8],
    // #50: live `STAT|<screen>:<page>` → retained `smol/<id>/status` (empty ⇒ none).
    status: &[u8],
    // #21 leaf-relay: `Some` on a gateway flush → wildcard-subscribe + cache leaf
    // configs for the ESP-NOW relay; `None` otherwise (see `mqtt_session`).
    cfg_cache: Option<&mut CfgCache>,
    // #50b: `Some` on a gateway flush → republish cached leaf statuses (see `mqtt_session`);
    // `None` otherwise.
    stat_cache: Option<&CfgCache>,
    // #70/#49: this node's own DIAG record → retained `smol/<id>/diag` (empty ⇒ none; see `mqtt_session`).
    diag: &[u8],
    // #70/#49: `Some` on a gateway flush → republish cached relayed diags (see `mqtt_session`); `None` otherwise.
    diag_cache: Option<&RelayCache>,
    // #71: this node's own one-shot WiFi-scan record → retained `smol/<id>/scan` (empty ⇒ none).
    scan: &[u8],
    // #71: `Some` on a gateway flush → republish cached relayed scans; `None` otherwise.
    scan_cache: Option<&RelayCache>,
    // #71: filled with the scan commands seen on `smol/+/cmd/scan` this burst (one-shot relay below).
    scan_req: &mut ScanReq,
    // #40: on a gateway flush, filled with `(leaf_id, staged announce)` when a leaf install
    // is pending → the caller relays it over ESP-NOW. `&mut None` on boot/leaf bursts.
    leaf_ota: &mut Option<(u8, crate::ota::Announce)>,
    // #40: caller-persisted last raw staged announce (see `mqtt_session`). `&mut None` on
    // boot/leaf bursts.
    staged_raw: &mut Option<crate::ota::Announce>,
    // #40: last relay attempt's `(leaf_id, phase, clear)` → published to `smol/<leaf>/ota/diag`
    // (see `mqtt_session`). `&mut None` on boot/leaf bursts.
    leaf_diag: &mut Option<(u8, &'static str, bool, u8)>,
    // #3: last relay attempt's RX evidence → published to `smol/<leaf>/ota/relaydiag` (see
    // `mqtt_session`). `&mut None` on boot/leaf bursts.
    leaf_relay_rx: &mut Option<RelayDiag>,
    // #139-followup: on a failed SELF-fetch, `(chunk_k, chunk_n, retries, stalls, where)` → formatted +
    // published retained to `smol/<id>/ota/diag` (see `mqtt_session`). `&mut None` otherwise.
    // #147: 5th field = the `ota_fail::*` code for the stage that died.
    ota_self_fail: &mut Option<(u32, u32, u32, u32, u32)>,
    tick: &mut dyn FnMut() -> bool,
) -> bool {
    let mut iface = create_interface(device);
    // #26 cast adds one UDP socket (the WLED pixel-stream) to the set.
    #[cfg(not(feature = "cast"))]
    let mut sockets_storage: [SocketStorage; 3] = Default::default();
    #[cfg(feature = "cast")]
    let mut sockets_storage: [SocketStorage; 4] = Default::default();
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

    // #9 item-1: throwaway UDP socket used ONLY to pre-warm the next-hop (router) ARP
    // right after DHCP (see the warm-up below). Tiny buffers — stack-negligible next to
    // the 512 B TCP buffers above, so the F1/F2 frame headroom is preserved. mqtt_session
    // never touches it.
    let mut warm_rx_meta = [udp::PacketMetadata::EMPTY; 1];
    let mut warm_tx_meta = [udp::PacketMetadata::EMPTY; 1];
    let mut warm_rx = [0u8; 1];
    let mut warm_tx = [0u8; 4];
    let warm_socket = udp::Socket::new(
        udp::PacketBuffer::new(&mut warm_rx_meta[..], &mut warm_rx[..]),
        udp::PacketBuffer::new(&mut warm_tx_meta[..], &mut warm_tx[..]),
    );
    let warm_handle = sockets.add(warm_socket);

    // #26 cast: a real UDP socket for the WLED DNRGB pixel-stream (present only in a
    // cast build). TX sized for one full DNRGB chunk (4 + 3*128 = 388 B ⇒ 512 with
    // margin); RX tiny (WLED never replies to realtime frames). Streamed AFTER the
    // MQTT session below, reusing this still-associated interface.
    #[cfg(feature = "cast")]
    let mut cast_rx_meta = [udp::PacketMetadata::EMPTY; 1];
    #[cfg(feature = "cast")]
    let mut cast_tx_meta = [udp::PacketMetadata::EMPTY; 4];
    #[cfg(feature = "cast")]
    let mut cast_rx = [0u8; 4];
    #[cfg(feature = "cast")]
    let mut cast_tx = [0u8; 512];
    #[cfg(feature = "cast")]
    let cast_handle = {
        let s = udp::Socket::new(
            udp::PacketBuffer::new(&mut cast_rx_meta[..], &mut cast_rx[..]),
            udp::PacketBuffer::new(&mut cast_tx_meta[..], &mut cast_tx[..]),
        );
        sockets.add(s)
    };

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
    crate::net::assert_max_tx_power(); // #141

    // #64: capture the WiFi-uplink RSSI HERE — the STA is confirmed connected (the loop
    // above waited for is_connected()==Ok(true)), so esp_wifi_sta_get_rssi has a live
    // association to read. The old #51 capture ran AFTER the whole burst returned, where
    // the STA state was unreliable, so it errored and my_rssi stayed at its -99 sentinel
    // (dead election tiebreak + no #64 publish). Set elect.my_rssi so mqtt_session
    // publishes it this same burst and the caller persists it for #51's tiebreak.
    if let Ok(r) = controller.rssi() {
        elect.my_rssi = r.clamp(-127, 0) as i8;
    }

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
            // #9 item-1: pre-warm the next-hop (router) ARP in a tight, bounded poll so
            // the timed MQTT TCP connect below is not delayed by a COLD first-ARP
            // round-trip — which occasionally overran the 15 s flush window and forced a
            // 30 s retry (the interface is rebuilt each flush → empty neighbour cache).
            // A throwaway datagram to the router's discard port (9) triggers neighbour
            // discovery; poll it out over ≤300 ms (and never past `deadline`). Purely
            // additive: if it does not resolve, the connect still does its own ARP —
            // identical to prior behavior, just without the warm cache.
            if let Some(router) = router {
                {
                    let s = sockets.get_mut::<udp::Socket>(warm_handle);
                    let _ = s.bind(49152 + (rng.random() % 16384) as u16);
                    let _ = s.send_slice(b"warm", (IpAddress::Ipv4(router), 9u16));
                }
                let warm_cap = Instant::now() + Duration::from_millis(300);
                while Instant::now() < warm_cap && Instant::now() < deadline {
                    iface.poll(smoltcp_now(), device, &mut sockets);
                }
            }
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
    // #26 cast: the result is bound (not tail-returned) so the cast stream can run
    // BETWEEN the session and the return; in a non-cast build that stream is cfg'd
    // away, leaving a `let … ; return` shape — a cfg-conditional `let_and_return`.
    #[allow(clippy::let_and_return)]
    let session_ok = mqtt_session(
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
        gw_own, // #48: forward the gateway-own keyed-config capture
        reset_req, // #52: forward the remote-reboot command capture
        install_requested,
        leaf_install_seen, // #40 #1: forward the leaf-install-seen latch
        peers, // #27: forward the caller's serialized roster to publish retained
        status, // #50: forward the live STAT|screen:page for smol/<id>/status
        cfg_cache, // #21: forward the gateway's leaf-config cache (or None)
        stat_cache, // #50b: forward the gateway's cached leaf statuses (or None)
        diag, // #70/#49: forward this node's own DIAG record (or empty)
        diag_cache, // #70/#49: forward the gateway's cached relayed diags (or None)
        scan, // #71: forward this node's own scan record (or empty)
        scan_cache, // #71: forward the gateway's cached relayed scans (or None)
        scan_req, // #71: forward the scan-command capture (one-shot relay)
        leaf_ota, // #40: forward the leaf-OTA install pairing (or &mut None)
        staged_raw, // #40: forward the persistent staged announce (or &mut None)
        leaf_diag, // #40: forward the diag/clear state (or &mut None)
        leaf_relay_rx, // #3: forward the relay RX-diag (or &mut None)
        ota_self_fail, // #139-followup: forward the self-fetch-fail snapshot (or &mut None)
        deadline,
        tick,
    );

    // #100 Stage 2: feed the broker-override fallback. We only reach here AFTER the WiFi
    // association succeeded (the assoc wait above returns early on failure), so `session_ok`
    // reflects the broker CONNACK, not a WiFi flap. A no-op unless a CFG-`B` override is active.
    note_broker_connect(session_ok);

    // #26 cast: if HA enabled it (learned via the retained `smol/<id>/cast` topic during
    // the session above), stream the mirrored glass image to the WLED matrix over the
    // STILL-LIVE association — the MQTT DISCONNECT only closed the TCP socket, not the STA
    // link (the caller re-pins ESP-NOW after we return). Bounded + long-press-abortable.
    #[cfg(feature = "cast")]
    if crate::net::cast::is_enabled() {
        let cast_port = 49152 + (rng.random() % 16384) as u16;
        cast_stream(
            &mut iface,
            device,
            &mut sockets,
            cast_handle,
            cast_port,
            deadline,
            tick,
        );
    }

    session_ok
}

/// #26 cast — hold the (already-associated) STA and stream the mirrored glass image to
/// the WLED matrix as DNRGB realtime UDP frames at ~10 fps for a bounded window. The
/// single radio is WiFi-committed (mesh-deaf) for the hold — the documented one-radio
/// cost, same as any burst — so it is short and long-press-abortable (`tick`). WLED
/// reverts to its normal render `DEFAULT_TIMEOUT_S` after the last frame, so simply
/// ceasing to stream releases the matrix; no explicit off-frame is needed. NOTE: `main`
/// is blocked in the flush while this runs, so the streamed image is the last-rendered
/// glass (a per-flush snapshot); continuous live-motion mirroring is the coexist
/// follow-on (it needs a persistent interleaved poll loop, which smol does not run).
#[cfg(feature = "cast")]
const CAST_HOLD: Duration = Duration::from_millis(3000);
/// ~10 fps — modest so the single radio isn't monopolised harder than a normal flush.
#[cfg(feature = "cast")]
const CAST_FRAME_INTERVAL: Duration = Duration::from_millis(100);

#[cfg(feature = "cast")]
fn cast_stream(
    iface: &mut Interface,
    device: &mut esp_wifi::wifi::WifiDevice<'static>,
    sockets: &mut SocketSet,
    cast_handle: smoltcp::iface::SocketHandle,
    src_port: u16,
    burst_deadline: Instant,
    tick: &mut dyn FnMut() -> bool,
) {
    use crate::net::cast;
    let cfg = cast::MatrixCfg {
        w: crate::secrets::WLED_CAST_W,
        h: crate::secrets::WLED_CAST_H,
        serpentine: crate::secrets::WLED_CAST_SERPENTINE,
        flip180: crate::secrets::WLED_CAST_FLIP180,
        thresh_pct: crate::secrets::WLED_CAST_THRESH_PCT,
        on: crate::secrets::WLED_CAST_ON,
        off: crate::secrets::WLED_CAST_OFF,
        timeout_s: cast::DEFAULT_TIMEOUT_S,
    };
    let host = crate::secrets::WLED_CAST_HOST;
    let dst = (
        IpAddress::Ipv4(Ipv4Addr::new(host[0], host[1], host[2], host[3])),
        cast::WLED_PORT,
    );

    // Bind the cast UDP socket once (ephemeral local port).
    {
        let s = sockets.get_mut::<udp::Socket>(cast_handle);
        if !s.is_open() && s.bind(src_port).is_err() {
            log::warn!("smol #26: cast socket bind failed");
            return;
        }
    }

    let hold_deadline = {
        let cap = Instant::now() + CAST_HOLD;
        if cap < burst_deadline {
            cap
        } else {
            burst_deadline
        }
    };
    let total = cfg.total();
    let mut next_frame = Instant::now();
    let mut frames = 0u32;
    loop {
        if tick() {
            break; // long-press → free the radio
        }
        iface.poll(smoltcp_now(), device, sockets);
        if Instant::now() >= next_frame {
            // One frame = all DNRGB chunks covering LEDs 0..total.
            let mut start = 0usize;
            let mut pkt = [0u8; 4 + 3 * cast::MAX_LEDS_PER_PKT];
            while start < total {
                let Some((n, next)) =
                    cast::with_frame(|m| cast::pack_dnrgb(m, &cfg, start, &mut pkt))
                else {
                    break;
                };
                {
                    let s = sockets.get_mut::<udp::Socket>(cast_handle);
                    let _ = s.send_slice(&pkt[..n], dst);
                }
                iface.poll(smoltcp_now(), device, sockets); // push the datagram out
                start = next;
            }
            frames += 1;
            next_frame = Instant::now() + CAST_FRAME_INTERVAL;
        }
        if Instant::now() >= hold_deadline {
            break;
        }
    }
    log::info!(
        "smol #26: cast streamed {} frame(s) to WLED {}.{}.{}.{} ({}x{})",
        frames,
        host[0],
        host[1],
        host[2],
        host[3],
        cfg.w,
        cfg.h
    );
}

/// OTA download budget. Unlike a ~1 s telemetry flush, the OTA burst is mesh-DEAF for
/// the whole ~0.6 MB HTTP download (spec §6-R4), so the window is minutes-scale. It is
/// user/announce-initiated + abortable (`tick` long-press), never auto-fleet-wide.
#[cfg(feature = "espnow")]
// OTA throughput fix (lucid's OTA-proof: engine passed to the download, then the
// 655 KB body clipped the old 180 s budget at <3.6 KB/s — a WINDOW-bound throughput
// bug, not reachability). Root cause: the 1536 B rx SocketBuffer (below) advertised a
// tiny TCP window, so the transfer was round-trip-bound. Primary fix = the 4 KB rx
// window + a prompt post-recv poll (below); this raised budget is the BACKSTOP: at the
// expected post-fix rate (~10-18 KB/s) a full image lands in <70 s, so 300 s is a
// comfortable ~4-8× margin without being recklessly long for the mesh-deaf window.
const OTA_FETCH_BUDGET: Duration = Duration::from_secs(300);

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

/// #188: best-effort publish of ONE retained `smol/<id>/ota/progress` = `<done>|<total>|<phase>`
/// mid-fetch, so the visualizers animate live progress AND — the diagnostic we lacked all night —
/// the LAST retained value pins exactly WHERE a transfer died (vs hand-correlating server GET logs).
/// REUSES the fetch's tcp socket in its between-chunks idle window: abort → connect broker →
/// CONNECT/CONNACK → PUBLISH(retain) → close; the loop's #147 per-chunk recycle then re-cleans +
/// reconnects to the HTTP host (state-agnostic, so this can't corrupt the download). A short internal
/// deadline bounds a broker hiccup so telemetry NEVER stalls the fetch; ANY failure is swallowed
/// (progress is best-effort). Shares the fetch's `tick` so a long-press still aborts. Broker leg =
/// `active_broker()` (#100). espnow-only (the OTA fetch is).
#[cfg(feature = "espnow")]
#[allow(clippy::too_many_arguments)]
fn publish_ota_progress(
    iface: &mut Interface,
    device: &mut esp_wifi::wifi::WifiDevice,
    sockets: &mut SocketSet,
    tcp_handle: smoltcp::iface::SocketHandle,
    src_port: u16,
    node_id: u8,
    done: u32,
    total: u32,
    phase: &str,
    tick: &mut dyn FnMut() -> bool,
) {
    let (bip, bport) = active_broker();
    let o = bip.octets();
    let broker = (IpAddress::Ipv4(smoltcp::wire::Ipv4Address::new(o[0], o[1], o[2], o[3])), bport);
    let deadline = Instant::now() + Duration::from_secs(3);
    // Force the (possibly lingering) socket to CLOSED, then open a fresh broker connection. The
    // fetch loop's per-chunk recycle re-cleans + reconnects to the HTTP host after we return.
    sockets.get_mut::<tcp::Socket>(tcp_handle).abort();
    loop {
        iface.poll(smoltcp_now(), device, sockets);
        if sockets.get_mut::<tcp::Socket>(tcp_handle).state() == tcp::State::Closed {
            break;
        }
        if Instant::now() > deadline {
            return;
        }
    }
    if sockets
        .get_mut::<tcp::Socket>(tcp_handle)
        .connect(iface.context(), broker, src_port)
        .is_err()
    {
        return;
    }
    loop {
        if tick() {
            return;
        }
        iface.poll(smoltcp_now(), device, sockets);
        match sockets.get_mut::<tcp::Socket>(tcp_handle).state() {
            tcp::State::Established => break,
            tcp::State::Closed => return,
            _ => {}
        }
        if Instant::now() > deadline {
            return;
        }
    }
    let mut pkt = [0u8; 128];
    // CONNECT (client id distinct from the flush session's `smol-<id>` so the broker doesn't
    // take-over that session's connection).
    let mut cid = MqttScratch::new();
    let _ = write!(cid, "smol-{}op", node_id);
    match crate::net::mqtt::encode_connect(
        &mut pkt,
        cid.as_bytes(),
        crate::secrets::MQTT_USER.as_bytes(),
        crate::secrets::MQTT_PASS.as_bytes(),
    ) {
        Some(n) if tcp_send(iface, device, sockets, tcp_handle, &pkt[..n], deadline, tick) => {}
        _ => return,
    }
    // CONNACK (require rc=0) — small local accumulator. Copy scalars out of the match BEFORE the
    // `copy_within` compaction so the `acc` borrow (via the parsed packet) is released first.
    let mut acc = [0u8; 64];
    let mut acc_len = 0usize;
    let mut connected = false;
    while !connected {
        if tick() {
            return;
        }
        iface.poll(smoltcp_now(), device, sockets);
        recv_into(sockets, tcp_handle, &mut acc, &mut acc_len);
        loop {
            let (consumed, ok, bad) = match crate::net::mqtt::parse_packet(&acc[..acc_len]) {
                None => break,
                Some((crate::net::mqtt::Incoming::ConnAck { return_code }, consumed)) => {
                    (consumed, return_code == 0, return_code != 0)
                }
                Some((_, consumed)) => (consumed, false, false),
            };
            acc.copy_within(consumed..acc_len, 0);
            acc_len -= consumed;
            if bad {
                return;
            }
            if ok {
                connected = true;
                break;
            }
        }
        if Instant::now() > deadline {
            return;
        }
    }
    // PUBLISH the retained progress line.
    let mut topic = MqttScratch::new();
    let _ = write!(topic, "smol/{}/ota/progress", node_id);
    let mut payload = MqttScratch::new();
    let _ = write!(payload, "{}|{}|{}", done, total, phase);
    if let Some(n) =
        crate::net::mqtt::encode_publish(&mut pkt, topic.as_bytes(), payload.as_bytes(), true)
    {
        let _ = tcp_send(iface, device, sockets, tcp_handle, &pkt[..n], deadline, tick);
    }
    // Close cleanly; the loop's recycle will re-establish the HTTP connection for the next chunk.
    sockets.get_mut::<tcp::Socket>(tcp_handle).close();
    iface.poll(smoltcp_now(), device, sockets);
}

/// #6 OTA FETCH burst (`espnow` gateway, triggered by a gated announce): stream the
/// announced image over plain HTTP/1.0 into the INACTIVE slot, hashing on the fly,
/// verify SHA-256 + size, then activate + reboot. The caller has ALREADY
/// `switch(Mode::WifiSta)`'d. `tick` keeps the UI alive + latches a long-press ABORT
/// (the whole download is mesh-deaf, §6-R4). Returns only on a NON-activating outcome
/// (failure/abort, good slot untouched); on SUCCESS it reboots inside `ota::activate`
/// and never returns.
#[cfg(feature = "espnow")]
#[allow(clippy::too_many_arguments)] // +fail diag (#139-followup) tips this to 8 params
pub fn run_ota_fetch(
    controller: &mut esp_wifi::wifi::WifiController<'static>,
    device: &mut esp_wifi::wifi::WifiDevice<'static>,
    mut rng: Rng,
    announce: &crate::ota::Announce,
    tick: &mut dyn FnMut() -> bool,
    // #40 relay-mode: when true, stage+verify the image to the inactive slot but do NOT
    // activate — hand the staged slot back via `staged_slot` so a GATEWAY can relay FROM
    // it to a leaf (no gateway reboot). Self-OTA passes `false` + `&mut None` → the fetch
    // body is byte-identical, only the terminal action differs (activate-reboot vs return).
    relay_mode: bool,
    staged_slot: &mut Option<esp_bootloader_esp_idf::ota::Slot>,
    // #139-followup observability: on a genuine self-fetch FAILURE (not a user abort), set to
    // `(chunk_k, chunk_n, retries, stalls)` — how far the download got + the transfer-trouble
    // counters. The self-OTA caller (`run_ota_update`) formats + publishes it retained to
    // `smol/<id>/ota/diag` (#135 armdiag pattern); release images are serial-silent, so this is
    // the ONLY fleet-visible signal of WHY a self-fetch failed.
    // #147: 5th field = the `ota_fail::*` code pinning the exact stage that died.
    fail: &mut Option<(u32, u32, u32, u32, u32)>,
    // #153: written each chunk so `main`'s OTA tick can paint the 1-px bottom progress
    // edge (bytes downloaded / image size). UI-agnostic: net/ only sets the counts, the
    // display lives in `main`. Shared by self-OTA and the gateway's relay-fetch phase.
    progress: &core::cell::Cell<crate::ota::OtaProgress>,
    // #188: Some(target_id) → publish a throttled retained `smol/<target_id>/ota/progress` during the
    // fetch (self id for a self-OTA; the LEAF id for a crown relay-fetch). None → no publish and the
    // fetch path is byte-identical to before (relay-unchanged / default-build invariant).
    progress_id: Option<u8>,
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
    // OTA throughput fix: 4 KB rx window (was 1536 B). The download was round-trip-bound
    // — the server sent one window's worth then waited a full RTT for the window to
    // reopen on the next poll, capping throughput at ~1536 B / cycle. 4096 nearly triples
    // the in-flight window (→ ~2.6× fewer stalls). tx stays 512 B — the request is small.
    //
    // F2 (oracle): the 4 KB rx buffer lives in a `static`, NOT on the stack. On the stack
    // it + `ImageWriter.stage` (also now static) + tcp_tx + header_buf ≈ 9.2 KB overflowed
    // the 8 KB task stack on the download. `run_ota_fetch` is ONE-SHOT + single-caller
    // (mesh-deaf, reboots on success, never re-entered concurrently), so a `static mut`
    // buffer is alias-safe — the previous borrow always ends when the fn returns before
    // any next call. `addr_of_mut!` avoids the reference-to-`static mut` lint.
    static mut OTA_TCP_RX: [u8; 4096] = [0; 4096];
    let tcp_rx: &mut [u8; 4096] = unsafe { &mut *core::ptr::addr_of_mut!(OTA_TCP_RX) };
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
            *fail = Some((0, 0, 0, 0, ota_fail::ASSOC)); // #139/#147: died before the download (association)
            log::warn!("smol OTA: WiFi association timed out");
            return false;
        }
    }
    let _ = controller.set_power_saving(esp_wifi::config::PowerSaveMode::None);
    crate::net::assert_max_tx_power(); // #141

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
            *fail = Some((0, 0, 0, 0, ota_fail::DHCP)); // #139/#147: died before the download (DHCP)
            log::warn!("smol OTA: DHCP timed out");
            return false;
        }
    }

    // Open the inactive-slot writer ONCE (image is streamed here across chunks, never buffered
    // whole). `writer.written()` doubles as the RESUME cursor and the running-SHA position.
    let Some(mut writer) = crate::ota::ImageWriter::begin() else {
        *fail = Some((0, 0, 0, 0, ota_fail::SLOT)); // #139/#147: died before the download (slot open)
        log::error!("smol OTA: cannot open inactive slot (no OTA partition table?)");
        return false;
    };
    let target = writer.target();

    // #138: fetch the image as sequential HTTP Range chunks instead of one fragile minutes-long
    // GET. Each chunk is its own short HTTP/1.0 GET+`Range` on a FRESH connection (Connection:
    // close) — many reliable short transfers inside the socket window that demonstrably works
    // (seconds-long MQTT/DHCP on the same association are rock-solid; only the single long GET
    // died mid-body — nebula finding 1b / IDF esp_https_ota partial_http_download). The running
    // SHA-256 composes across chunks (the writer accumulates), so the verify gate below is
    // UNCHANGED. A broken chunk resumes from `writer.written()` rather than restarting the whole
    // image; `OTA_FETCH_BUDGET` stays the overall cap. `chunk_retries` is surfaced to serial
    // (the fetch reboots on success, so an in-RAM diag counter wouldn't survive — serial is the
    // rig's forensic sink).
    const OTA_CHUNK: u32 = 48 * 1024;
    /// Consecutive zero-progress attempts before giving up (livelock guard inside the budget).
    const OTA_MAX_STALL: u32 = 6;
    /// #147: bounded per-chunk connect+handshake window. Chunk N+1 reuses the smoltcp socket right
    /// after chunk N's `Connection: close`; if that reconnect wedges (SYN dropped / server still
    /// holding the old half-open / stack mid-recycle) the handshake wait must fail FAST into the
    /// stall guard and retry on a FRESH ephemeral port — NOT spin against the 300 s global budget.
    /// That was the #147 bug: one wedged chunk 2 ate the whole fetch (11 attempts, each 206-served
    /// chunk 1 then a budget death). 8 s ≫ a healthy LAN handshake, so a real slow assoc still wins.
    const OTA_CHUNK_CONNECT: Duration = Duration::from_secs(8);

    // Variable-width decimal into `out` (Range byte-positions must NOT be zero-padded for all
    // servers); returns the digit count. `v` is a u32 → at most 10 digits.
    fn write_dec(mut v: u32, out: &mut [u8]) -> usize {
        if v == 0 {
            out[0] = b'0';
            return 1;
        }
        let mut tmp = [0u8; 10];
        let mut i = 0;
        while v > 0 {
            tmp[i] = b'0' + (v % 10) as u8;
            v /= 10;
            i += 1;
        }
        for j in 0..i {
            out[j] = tmp[i - 1 - j];
        }
        i
    }

    let mut range_ok = true; // flips false if chunk 0 returns 200 (server ignored Range)
    let mut chunk_retries: u32 = 0; // re-requests forced by a short/failed chunk
    let mut stall: u32 = 0; // consecutive zero-progress attempts
    // #188: throttle for the live MQTT progress publish. Seeded to NOW so the first loop fires it
    // immediately (a fetch that dies fast still leaves a death-point); ~5 s cadence after that so the
    // telemetry never dominates the download's airtime.
    let mut next_progress_pub = Instant::now();
    let chunk_n = announce.size.div_ceil(OTA_CHUNK); // total chunks (for the #139-followup fail diag)
    // #147: the stage the CURRENT chunk attempt is in — advanced as we pass connect → handshake →
    // send → recv, so whatever return fires below carries the precise failure point in the diag.
    let mut fail_point: u32 = ota_fail::NONE;

    while writer.written() < announce.size {
        // #139-followup/#147: keep the fail-diag snapshot current so ANY failure return below carries
        // how far the download got + the trouble counters + the stage it died in (the self-OTA
        // caller publishes it retained to smol/<id>/ota/diag — release images are serial-silent, so
        // this is the fleet-visible why).
        *fail = Some((writer.written() / OTA_CHUNK, chunk_n, chunk_retries, stall, fail_point));
        // #153: surface live byte progress for the UI tick's bottom edge.
        progress.set(crate::ota::OtaProgress { done: writer.written(), total: announce.size });
        // #188: throttled live progress → retained MQTT (viz + the death-point diagnostic). Best-effort;
        // reuses the tcp socket in this between-chunks idle window (the per-chunk recycle below re-cleans
        // + reconnects to the HTTP host). None ⇒ skipped entirely (byte-identical to the pre-#188 path).
        if let Some(pid) = progress_id {
            if Instant::now() >= next_progress_pub {
                next_progress_pub = Instant::now() + Duration::from_secs(5);
                let src_port = 49152 + (rng.random() % 16384) as u16;
                let phase = if relay_mode { "relayfetch" } else { "self" };
                publish_ota_progress(
                    &mut iface, device, &mut sockets, tcp_handle, src_port, pid,
                    writer.written(), announce.size, phase, tick,
                );
            }
        }
        if tick() {
            log::warn!("smol OTA: aborted by long-press (slot untouched)");
            return false;
        }
        if Instant::now() > deadline {
            *fail = Some((writer.written() / OTA_CHUNK, chunk_n, chunk_retries, stall, ota_fail::DEADLINE));
            log::warn!("smol OTA: download timed out (slot untouched)");
            return false;
        }
        let off = writer.written();
        let end = core::cmp::min(off + OTA_CHUNK, announce.size) - 1; // inclusive last byte

        // #147: recycle the socket to a genuinely connectable state before this chunk. smoltcp's
        // `abort()` sets CLOSED *synchronously* but leaves the RST queued (it egresses only on a
        // later poll) and the stale 4-tuple/buffers in place until `connect()`'s `reset()` wipes
        // them. On-air the single post-abort poll wasn't reliably draining that, so chunk 2+
        // reconnected on a half-recycled handle and wedged. Pump the interface until the socket
        // reports CLOSED (RST flushed) with a bounded wait; if it never does, count a stall and
        // retry the whole chunk (fresh abort + new port) rather than reconnect on a dirty handle.
        fail_point = ota_fail::RECYCLE;
        {
            let s = sockets.get_mut::<tcp::Socket>(tcp_handle);
            s.abort();
        }
        {
            let recycle_deadline = Instant::now() + OTA_CHUNK_CONNECT;
            let mut recycled = false;
            loop {
                iface.poll(smoltcp_now(), device, &mut sockets);
                if sockets.get_mut::<tcp::Socket>(tcp_handle).state() == tcp::State::Closed {
                    recycled = true;
                    break; // connectable
                }
                if tick() {
                    return false;
                }
                if Instant::now() > recycle_deadline || Instant::now() > deadline {
                    break; // couldn't fully recycle in-window
                }
            }
            if !recycled {
                chunk_retries += 1;
                stall += 1;
                if stall >= OTA_MAX_STALL {
                    *fail = Some((writer.written() / OTA_CHUNK, chunk_n, chunk_retries, stall, ota_fail::RECYCLE));
                    log::error!("smol OTA: socket would not recycle to a connectable state — aborting (slot untouched)");
                    return false;
                }
                continue; // fail_point=RECYCLE rides the loop-top snapshot on the retry
            }
        }
        // Fresh ephemeral port each chunk (avoids TIME_WAIT / server half-open collisions on reuse).
        let src_port = 49152 + (rng.random() % 16384) as u16;
        fail_point = ota_fail::CONNECT;
        {
            let s = sockets.get_mut::<tcp::Socket>(tcp_handle);
            if s.connect(iface.context(), (IpAddress::Ipv4(ip), port), src_port)
                .is_err()
            {
                chunk_retries += 1;
                stall += 1;
                if stall >= OTA_MAX_STALL {
                    *fail = Some((writer.written() / OTA_CHUNK, chunk_n, chunk_retries, stall, ota_fail::CONNECT));
                    log::error!("smol OTA: repeated TCP connect failures — aborting (slot untouched)");
                    return false;
                }
                continue;
            }
        }
        // #147: BOUNDED handshake. A reused socket that wedges HERE (connect ok, may_send never
        // true) must retry on a fresh port via the stall guard instead of spinning to the 300 s
        // global budget — that unbounded wait was the #147 whole-fetch death (one wedged chunk 2
        // ate the entire budget, 11×). Only the GLOBAL deadline or the stall cap is terminal; the
        // per-chunk window just recycles.
        fail_point = ota_fail::HANDSHAKE;
        {
            let chunk_connect_deadline = core::cmp::min(Instant::now() + OTA_CHUNK_CONNECT, deadline);
            let mut connected = false;
            let mut window_elapsed = false;
            while !connected {
                iface.poll(smoltcp_now(), device, &mut sockets);
                if sockets.get_mut::<tcp::Socket>(tcp_handle).may_send() {
                    connected = true;
                } else if tick() {
                    return false;
                } else if Instant::now() > deadline {
                    *fail = Some((writer.written() / OTA_CHUNK, chunk_n, chunk_retries, stall, ota_fail::HANDSHAKE));
                    log::warn!("smol OTA: TCP connect timed out (slot untouched)");
                    return false;
                } else if Instant::now() > chunk_connect_deadline {
                    window_elapsed = true;
                    break; // per-chunk handshake window elapsed → retry on a fresh port
                }
            }
            if window_elapsed {
                chunk_retries += 1;
                stall += 1;
                if stall >= OTA_MAX_STALL {
                    *fail = Some((writer.written() / OTA_CHUNK, chunk_n, chunk_retries, stall, ota_fail::HANDSHAKE));
                    log::error!("smol OTA: repeated TCP handshake wedges — aborting (slot untouched)");
                    return false;
                }
                continue;
            }
        }

        // Request the byte range [off, end]. Sent in pieces (no format buffer; « the 512 B tx ring).
        // `Range: bytes=<off>-<end>` — a 206-capable server returns exactly that slice; a server
        // that ignores Range answers 200 with the whole body (handled as a fallback on chunk 0).
        let mut rbuf = [0u8; 32];
        let mut rl = 6;
        rbuf[..6].copy_from_slice(b"bytes=");
        rl += write_dec(off, &mut rbuf[rl..]);
        rbuf[rl] = b'-';
        rl += 1;
        rl += write_dec(end, &mut rbuf[rl..]);
        {
            fail_point = ota_fail::SEND;
            let s = sockets.get_mut::<tcp::Socket>(tcp_handle);
            let ok = s.send_slice(b"GET ").is_ok()
                && s.send_slice(path.as_bytes()).is_ok()
                && s.send_slice(b" HTTP/1.0\r\nHost: ").is_ok()
                && s.send_slice(host.as_bytes()).is_ok()
                && s.send_slice(b"\r\nRange: ").is_ok()
                && s.send_slice(&rbuf[..rl]).is_ok()
                && s.send_slice(b"\r\nConnection: close\r\n\r\n").is_ok();
            if !ok {
                chunk_retries += 1;
                stall += 1;
                if stall >= OTA_MAX_STALL {
                    *fail = Some((writer.written() / OTA_CHUNK, chunk_n, chunk_retries, stall, ota_fail::SEND));
                    log::error!("smol OTA: repeated request-send failures — aborting (slot untouched)");
                    return false;
                }
                continue;
            }
        }

        // Drain this chunk's response into the writer (streaming; headers validated once/chunk).
        let chunk_start = writer.written();
        let mut header_buf = [0u8; 512];
        let mut header_len = 0usize;
        let mut headers_done = false;
        let mut bad = false;
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
                                bad = true;
                                return (0, false); // headers exceed the buffer → give up
                            }
                            header_buf[header_len..header_len + take]
                                .copy_from_slice(&data[..take]);
                            header_len += take;
                            if let Some(bstart) = crate::ota::header_end(&header_buf[..header_len]) {
                                match crate::ota::status_code(&header_buf[..header_len]) {
                                    Some(206) => {} // Partial Content — Range honoured
                                    Some(200) if off == 0 => {
                                        // Server ignored Range → full-body fallback (the old
                                        // single-GET path). Validate Content-Length == size as
                                        // before; this GET is NOT resumable (checked after drain).
                                        range_ok = false;
                                        if let Some(cl) =
                                            crate::ota::content_length(&header_buf[..header_len])
                                        {
                                            if cl != announce.size {
                                                bad = true;
                                                return (take, false); // length mismatch → abort
                                            }
                                        }
                                    }
                                    _ => {
                                        bad = true; // non-206 range reply (or 200 mid-stream)
                                        return (take, false);
                                    }
                                }
                                headers_done = true;
                                let fed = writer.feed(&header_buf[bstart..header_len]);
                                if !fed {
                                    bad = true; // flash write error
                                }
                                return (take, fed);
                            }
                            (take, true) // headers still arriving
                        } else {
                            let fed = writer.feed(data);
                            if !fed {
                                bad = true; // flash write error
                            }
                            (data.len(), fed)
                        }
                    });
                    match outcome {
                        Ok(true) => {}
                        Ok(false) => closed = true, // bad header / flash error → end this chunk
                        Err(_) => closed = true,
                    }
                } else if !s.may_recv() {
                    closed = true; // peer closed (Connection: close) + rx drained → chunk complete
                }
            }
            // Poll AGAIN right after draining so the reopened window (+ its ACK) hits the wire
            // this iteration — halves the window-closed gap that made the transfer RTT-bound.
            iface.poll(smoltcp_now(), device, &mut sockets);
            if writer.written() >= announce.size {
                break; // whole image done
            }
            if closed {
                break; // this chunk's connection ended → outer loop resumes from written()
            }
            if Instant::now() > deadline {
                *fail = Some((writer.written() / OTA_CHUNK, chunk_n, chunk_retries, stall, ota_fail::DEADLINE));
                log::warn!("smol OTA: download timed out (slot untouched)");
                return false;
            }
        }

        if bad {
            *fail = Some((writer.written() / OTA_CHUNK, chunk_n, chunk_retries, stall, ota_fail::STATUS));
            log::error!("smol OTA: bad HTTP status/length on range {off}-{end} (slot untouched)");
            return false;
        }
        if !range_ok && writer.written() < announce.size {
            // A 200 server re-sends from 0, so an incomplete full-body GET can't be resumed
            // (that's the OLD failure mode). Fail cleanly rather than double-write.
            *fail = Some((writer.written() / OTA_CHUNK, chunk_n, chunk_retries, stall, ota_fail::FALLBACK));
            log::error!(
                "smol OTA: server ignored Range (200) and the single GET died mid-body — not resumable"
            );
            return false;
        }
        // Progress accounting: a chunk that advanced resets the stall guard; a chunk that
        // delivered fewer bytes than requested (broke early) forces re-requesting the remainder.
        if writer.written() > chunk_start {
            stall = 0;
            if range_ok && writer.written() <= end && writer.written() < announce.size {
                chunk_retries += 1; // short chunk → remainder re-requested next iteration
            }
        } else {
            chunk_retries += 1;
            stall += 1;
            if stall >= OTA_MAX_STALL {
                *fail = Some((writer.written() / OTA_CHUNK, chunk_n, chunk_retries, stall, ota_fail::STALL));
                log::error!(
                    "smol OTA: {OTA_MAX_STALL} zero-progress attempts at offset {off} — aborting (slot untouched)"
                );
                return false;
            }
        }
    }

    log::info!(
        "smol OTA: image received — {} B over Range chunks ({} chunk retries)",
        writer.written(),
        chunk_retries
    );

    // Integrity gate (exact size + SHA-256) AND #32 authenticity gate (Ed25519 over the
    // signed manifest "build|size|sha256hex"). BOTH must pass before otadata is touched;
    // either failure discards with the good slot still active. `finalize` runs FIRST
    // (flushes the last stage → the integrity proof); `verify_signature` is a pure,
    // fail-closed, panic-free check. Coverage: reaching here at all requires a 5-field
    // signed announce (`parse_announce` `splitn(5)`) → a MISSING sig is a 4-field announce
    // that never parses → never fetched (reject-missing = the parser). This gate is the
    // reject-BAD-sig half. Together: accept-good only. (#32 always-enforces — with strict
    // `splitn(5)` a require-off flag would only fail-OPEN on a bad 5-field sig, pointless;
    // the "deliver #32 UNSIGNED" rollout is a publish-format choice — a 4-field announce
    // pre-#32 boards parse — not a board flag.)
    if writer.finalize(announce.size, &announce.sha256)
        && crate::ota::verify_signature(announce.signed_msg(), announce.sig())
    {
        if relay_mode {
            // #40: the gateway staged+verified a leaf's image into ITS inactive slot; do
            // NOT activate (this board isn't the one being updated). Hand back the slot so
            // the relay reads it back chunk-by-chunk over ESP-NOW.
            log::info!("smol #40: relay image staged+VERIFIED in the inactive slot (not activated)");
            *staged_slot = Some(target);
            return true;
        }
        log::info!("smol OTA: image VERIFIED (SHA-256 + ed25519) — activating the new slot");
        crate::ota::activate(target, announce.build, false); // self-OTA → confirm via DHCP
        false // reached only if the otadata write failed
    } else {
        // #139-followup: the download COMPLETED but the integrity/authenticity gate rejected it
        // (corrupt/truncated/bad-sig). Record K=N (all chunks fetched) + at=verify so the fleet diag
        // distinguishes a verify failure from a mid-download death.
        *fail = Some((chunk_n, chunk_n, chunk_retries, stall, ota_fail::VERIFY));
        log::error!("smol OTA: verify FAILED (size/SHA-256 or ed25519 signature) — discarded (good slot intact)");
        false
    }
}
